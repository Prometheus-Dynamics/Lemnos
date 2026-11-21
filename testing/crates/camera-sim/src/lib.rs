//! Simulated hardware for the camera chain, and the real drivers over it.
//!
//! What is real: the host and `lemnosd`'s `Service`, the sensor, power switch and raw-bus drivers,
//! and the pisp front end and back end drivers (their code, manifests and streams); the host gives
//! the last two a simulated block through the `sim-hardware` hook of `lemnos-driver-pisp`. What is
//! simulated: the I2C buses (a 16-bit-register sensor model and two 8-bit chips), the GPIO line,
//! the receiver (a driver with the receiver driver's manifest that makes frames from the sensor
//! model's registers) and the two PiSP blocks ([`pisp`]).
//!
//! The kernel module's per-address I2C claim is modelled too (`claims`): one holder of `i2c-<bus>:0x<addr>`
//! at a time, held until the holder's bus handle is dropped.
//!
//! # Two ways to run it
//!
//! * [`Sim`] is an instance: its own service on its own socket, its own sensor, clock, receiver
//!   and switches, in a test process with any number of others. Its state is its own; nothing is
//!   shared between two instances, and the process keeps none of it. It has the receiver and the
//!   sensor only (no PiSP blocks, which the driver installs once per process). Its frames are paced
//!   on the sensor's frame period against the host's clock ([`Pace::Real`]), or granted one at a time
//!   ([`Pace::Lockstep`], with controls for a named frame), and its faults are switches on the
//!   instance ([`Sim::sensor_fault`], [`Sim::refuse_receiver_starts`], and the rest).
//! * The root functions here ([`world`], [`sensor_step`], [`time`], [`WORLD`]) act on the
//!   process's own chain, for the chain proof and the off-board daemon: one chain per process, the
//!   PiSP blocks included. That chain's frames are on demand (the default), on the sensor's steps
//!   ([`pisp::set_paced`]), or against the host's clock ([`set_pace`]).
//!
//! The frame period is the sensor's: its frame length in lines times the line time its registers
//! give ([`period_ns`]). Frame stamps are the sensor-start times on the host's `CLOCK_MONOTONIC`
//! (or on the injected clock of an instance): the start of the first frame is one period after the
//! receiver started, and frame `k` is stamped at `start + (k + 1) x period`, never 0.

pub mod chain;
pub mod pisp;
pub mod profile;
pub mod scene;
pub mod sim;
pub mod time;

use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};

use lemnos_contract_camera_isp::CameraIsp;
use lemnos_contract_camera_receiver::{
    CameraReceiver, Counters, FrameLayout, FrameStarts, Limits, LinkConfig, MAX_STREAM_NAME,
    Packing,
};
use lemnos_core::{
    Config, ConfigError, Dev, Health, Key, Kind, Lifecycle, Provider, Result, Status, StreamHandle,
    driver,
};
use lemnos_hal::ErrorKind;
use lemnos_hal::blocks::StreamFrame;
use lemnos_hal::i2c::{ErrorType, I2c as HalI2c, Operation};
use lemnos_host::{DriverEntry, entry};
use lemnos_sim::SimBoard;

pub use chain::{Chain, Control, Faults, Home, Meta, Pace, SensorFault};
pub use sim::{ClockSource, Consumer, Event, Memory, Refusal, Sim, SimBuilder, SimFrame};

use chain::LEGACY;

// ---------------------------------------------------------------- the sensor model
const SYSCLK_HZ: u128 = 96_000_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Model {
    M0,
    A,
    B1,
}

pub struct Target {
    wide: bool,
    pub regs: BTreeMap<u16, u8>,
    pointer: u16,
}

#[derive(Default)]
pub struct Bus {
    pub targets: BTreeMap<u8, Target>,
}

pub struct World {
    pub buses: BTreeMap<String, Bus>,
    claims: HashSet<(String, u8)>,
    claims_on: bool,
    model: Model,
    /// 0x0100 on i2c-10:0x60 is 1.
    pub streaming: bool,
    /// When the sensor last started streaming, on the chain's clock (ns; 0 before it has).
    pub started_at: u64,
    transactions: u64,
    /// Writes captured between the group hold's start and end, applied together at the launch.
    held: Option<BTreeMap<u16, u8>>,
    /// Between the group hold's start and its end: writes go to `held`.
    capturing: bool,
    /// Each time the sensor's exposure was launched (sim ns) and what it became, in sensor units
    /// (sixteenths of a line).
    launches: Vec<(u64, u32)>,
    /// The frame a launch shows in: a launch in frame F shows in frame F + `latch_frames`. Two is
    /// the sensor's; a stand-in that shows it a frame early (one) is how a test makes the latch fail.
    pub latch_frames: u64,
}

/// The process's own chain's world (see [`chain`]). A test sets it with [`new_world`].
pub static WORLD: &Mutex<Option<World>> = &LEGACY.world;

/// The module's exclusion, as `kernel/rs/rp1_csi2.rs` and `kernel/rs/pisp_fe.rs` make it: the
/// receiver's raw `frames` stream and the front end are never running together (the receiver's
/// `START` is refused with `EBUSY` while the front end streams, the front end's while a raw stream
/// runs), because in front-end mode the CSI-2 channel feeds the front end instead of memory. The
/// module takes the decision under one lock; here it is one compare-and-swap, so the later of two
/// starts always sees the earlier. A process holds one simulated chain, so this is process-wide;
/// [`new_world`] clears it. An instance has its own.
pub static EXCLUSIVE: &AtomicU8 = &LEGACY.exclusive;
/// [`EXCLUSIVE`]: nothing streams.
pub const NOBODY: u8 = 0;
/// [`EXCLUSIVE`]: the receiver's raw stream runs.
pub const RAW: u8 = 1;
/// [`EXCLUSIVE`]: the front end has been started with a job.
pub const FRONT_END: u8 = 2;

/// Takes the block for `who`, `Err` (`EBUSY`) if the other one has it. Taking it again is fine for
/// the front end (a restart); for the raw stream the receiver's own `running` check refuses it.
pub fn take(who: u8) -> Result<()> {
    LEGACY.take(who)
}

/// Lets go of the block if `who` holds it.
pub fn release(who: u8) {
    LEGACY.release(who);
}

/// Puts the process's own chain's frames on `pace` (the daemon's `--pace`). [`Pace::Demand`] is the
/// event-driven clock; [`Pace::Real`] paces the frames on the daemon's tick. The PiSP front ends
/// follow the sensor's steps when [`pisp::set_paced`] says so, whatever this is.
pub fn set_pace(pace: Pace) {
    LEGACY.set_pace(pace);
}

/// The process's own chain's world.
pub fn world<R>(f: impl FnOnce(&mut World) -> R) -> R {
    LEGACY.with_world(f)
}

fn put(regs: &mut BTreeMap<u16, u8>, at: u16, bytes: &[u8]) {
    for (i, b) in bytes.iter().enumerate() {
        regs.insert(at + i as u16, *b);
    }
}

/// The process's own chain's world, with the sensor and the bus claims as `claims_on` and `model`
/// say. Clears the exclusion of the raw stream and the front end.
pub fn new_world(claims_on: bool, model: Model) -> World {
    EXCLUSIVE.store(NOBODY, Ordering::SeqCst);
    new_world_in(claims_on, model)
}

/// A world with the chips of the reference board's camera bus and power, and no shared state.
#[must_use]
pub fn new_world_in(claims_on: bool, model: Model) -> World {
    let mut buses = BTreeMap::new();
    // i2c-10, 0x60: the OV9782 as the board's records show it at rest (BR1 2026-11-13, raze2-p2).
    let mut regs = BTreeMap::new();
    put(&mut regs, 0x300a, &[0x97, 0x82]);
    put(&mut regs, 0x382e, &[0x01]);
    put(&mut regs, 0x382c, &[0x05, 0xb0]);
    put(&mut regs, 0x380c, &[0x02, 0xd8]);
    put(&mut regs, 0x380e, &[0x11, 0x2b]);
    let mut bus10 = Bus::default();
    bus10.targets.insert(
        0x60,
        Target {
            wide: true,
            regs,
            pointer: 0,
        },
    );
    buses.insert("i2c-10".to_string(), bus10);
    // i2c-1: BMM150 chip id 0x32 at 0x40 of address 0x10; INA238 device id 0x2381 at 0x3f of 0x40.
    let mut bus1 = Bus::default();
    let mut mag = BTreeMap::new();
    put(&mut mag, 0x40, &[0x32]);
    bus1.targets.insert(
        0x10,
        Target {
            wide: false,
            regs: mag,
            pointer: 0,
        },
    );
    let mut ina = BTreeMap::new();
    put(&mut ina, 0x3f, &[0x23, 0x81]);
    bus1.targets.insert(
        0x40,
        Target {
            wide: false,
            regs: ina,
            pointer: 0,
        },
    );
    buses.insert("i2c-1".to_string(), bus1);
    World {
        buses,
        claims: HashSet::new(),
        claims_on,
        model,
        streaming: false,
        started_at: 0,
        transactions: 0,
        held: None,
        capturing: false,
        launches: Vec::new(),
        latch_frames: 2,
    }
}

impl World {
    /// The exposure the registers hold, in sixteenths of a line.
    fn exposure_regs(&self) -> u32 {
        let regs = &self.buses["i2c-10"].targets[&0x60].regs;
        let get = |at: u16| u32::from(*regs.get(&at).unwrap_or(&0));
        (get(0x3500) & 0x0f) << 16 | get(0x3501) << 8 | get(0x3502)
    }

    /// The level the frame whose start is `start_ns` shows: the exposure launched before the start
    /// of the frame before it. A group launched in frame F is latched at the start of F + 1, and
    /// its picture is read out in F + 2.
    pub fn level_for_frame_at(&self, start_ns: u64, period_ns: u64) -> u8 {
        let before =
            start_ns.saturating_sub(period_ns * self.latch_frames.max(1).saturating_sub(1));
        let units = self
            .launches
            .iter()
            .rev()
            .find(|(at, _)| *at < before)
            .map_or_else(|| self.launches.first().map_or(0, |l| l.1), |l| l.1);
        let lines = units / 16;
        u8::try_from((lines * 240 / 1800).clamp(8, 250)).unwrap_or(250)
    }
}

/// One sensor start, shared by every consumer of the sensor. The clock moves to the start; each
/// front end with a job queued makes its frame, and each with none drops the start; each running
/// receiver makes its frame (or drops it, with no buffer). Returns once every one has done so.
///
/// # Panics
/// A running receiver does not take the step within ten seconds: its daemon's tick is not running.
pub fn sensor_step() -> u64 {
    let start = LEGACY.sensor_start();
    pisp::step_front_ends(start);
    LEGACY.step_receivers(start);
    start
}

/// Makes the simulated sensor stop sending behind its driver: the mode select is written 0 on the
/// sensor's bus, as a register write from outside the driver would be, and nothing else changes.
/// The driver keeps its own state, so it still believes the sensor streams. Its silence watch is
/// what must notice (and restart the sensor). Sensor starts stepped while silent make no frames.
pub fn silence_sensor() {
    world(|w| {
        let target = w
            .buses
            .get_mut("i2c-10")
            .and_then(|bus| bus.targets.get_mut(&0x60))
            .expect("the simulated sensor is on i2c-10 at 0x60");
        target.regs.insert(0x0100, 0);
        w.streaming = false;
    });
}

/// The frame period in ns, from the model's registers (at least 1).
#[must_use]
pub fn period_ns() -> u64 {
    u64::try_from(period_ps() / 1000).unwrap_or(u64::MAX).max(1)
}

/// The frame period in ps from the model's registers.
pub fn period_ps() -> u128 {
    world(|w| period_ps_in(w))
}

/// The frame period in ps from a world's registers.
pub fn period_ps_in(w: &World) -> u128 {
    let regs = &w.buses["i2c-10"].targets[&0x60].regs;
    let word = |at: u16| {
        u128::from(*regs.get(&at).unwrap_or(&0)) << 8
            | u128::from(*regs.get(&(at + 1)).unwrap_or(&0))
    };
    let (hts, vts, global) = (word(0x380c), word(0x380e), word(0x382c));
    let sysclk = match w.model {
        Model::M0 => vts * hts,
        Model::A => (vts + 1) * hts,
        Model::B1 => (vts - 1) * hts + global,
    };
    // The system clock is the PLL2 multiplier's (24 MHz / 3 * multiplier / 8: RAW10's mode runs
    // 80 MHz, RAW8's 96 MHz). A chip that was never given one (the board's rest state) is at 96.
    let multiplier = u128::from(
        *regs
            .get(&lemnos_driver_ov9782::chip::PLL2_MULTIPLIER)
            .unwrap_or(&0),
    );
    let sysclk_hz = if multiplier == 0 {
        SYSCLK_HZ
    } else {
        24_000_000 / 3 * multiplier / 8
    };
    sysclk * 1_000_000_000_000 / sysclk_hz
}

// ---------------------------------------------------------------- the bus handle
/// One device's handle on a bus. With `--claims on` it behaves as `ModuleI2c`: the first transfer to an
/// address claims it, a second handle's transfer to a claimed address is `Busy`, an address that does not
/// answer is let go, the claims go with the handle.
struct Handle {
    bus: String,
    own: HashSet<u8>,
    home: Home,
}

impl ErrorType for Handle {
    type Error = ErrorKind;
}

impl HalI2c for Handle {
    fn transaction(
        &mut self,
        address: u8,
        ops: &mut [Operation<'_>],
    ) -> core::result::Result<(), ErrorKind> {
        let home = self.home.clone();
        home.chain().with_world(|w| {
            w.transactions += 1;
            if w.claims_on && !self.own.contains(&address) {
                if !w.claims.insert((self.bus.clone(), address)) {
                    return Err(ErrorKind::Busy);
                }
                self.own.insert(address);
            }
            if address == 0x60 && self.bus == "i2c-10" {
                // A sensor fault: failed answers until it is cleared; gone once, and spent.
                let mut faults = home.chain().faults();
                match faults.sensor {
                    Some(SensorFault::Failed(_)) => return Err(ErrorKind::Failed),
                    Some(SensorFault::Gone(_)) => {
                        faults.sensor = None;
                        return Err(ErrorKind::NotFound);
                    }
                    None => {}
                }
            }
            let streaming_before = w.streaming;
            let sensor = address == 0x60 && self.bus == "i2c-10";
            let mut held = w.held.take();
            let mut capturing = w.capturing;
            let mut launched = false;
            let bus = w.buses.get_mut(&self.bus).ok_or(ErrorKind::NotFound)?;
            let Some(t) = bus.targets.get_mut(&address) else {
                if self.own.remove(&address) {
                    w.claims.remove(&(self.bus.clone(), address));
                }
                return Err(ErrorKind::Nack);
            };
            let mut mode_select = None;
            for op in ops.iter_mut() {
                match op {
                    Operation::Write(data) => {
                        let w_ = if t.wide { 2 } else { 1 };
                        if data.len() < w_ {
                            return Err(ErrorKind::InvalidInput);
                        }
                        t.pointer = if t.wide {
                            u16::from(data[0]) << 8 | u16::from(data[1])
                        } else {
                            u16::from(data[0])
                        };
                        for b in &data[w_..] {
                            if sensor && t.pointer == 0x3208 {
                                // The group register: 0x00 starts a hold, 0xa0 launches what it caught.
                                match *b {
                                    0x00 => {
                                        held = Some(BTreeMap::new());
                                        capturing = true;
                                    }
                                    0x10 => capturing = false,
                                    0xa0 => {
                                        for (r, v) in held.take().unwrap_or_default() {
                                            t.regs.insert(r, v);
                                        }
                                        launched = true;
                                    }
                                    _ => {}
                                }
                            } else if let (true, true, Some(group)) =
                                (sensor, capturing, held.as_mut())
                            {
                                group.insert(t.pointer, *b);
                            } else {
                                t.regs.insert(t.pointer, *b);
                                launched |=
                                    sensor && !capturing && (0x3500..=0x3502).contains(&t.pointer);
                            }
                            if t.wide && t.pointer == 0x0100 {
                                mode_select = Some(*b);
                            }
                            t.pointer += 1;
                        }
                    }
                    Operation::Read(buf) => {
                        for b in buf.iter_mut() {
                            *b = *t.regs.get(&t.pointer).unwrap_or(&0);
                            t.pointer += 1;
                        }
                    }
                }
            }
            w.held = held;
            w.capturing = capturing;
            if launched {
                let units = w.exposure_regs();
                w.launches.push((home.chain().clock.now(), units));
            }
            if address == 0x60 && self.bus == "i2c-10" {
                if let Some(v) = mode_select {
                    w.streaming = v & 1 == 1;
                } else {
                    w.streaming = streaming_before;
                }
                if w.streaming && !streaming_before {
                    // The sensor's start, as the clock of its chain reads it: the frames fall from here.
                    w.started_at = home.chain().source_now();
                }
            }
            Ok(())
        })
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.home.chain().with_world(|w| {
            for a in self.own.drain() {
                w.claims.remove(&(self.bus.clone(), a));
            }
        });
    }
}

// ---------------------------------------------------------------- the backend
pub struct Board {
    pub lines: SimBoard,
    home: Home,
}

impl Provider for Board {
    fn provided(&self, need: &str) -> bool {
        self.home.chain().with_world(|w| w.buses.contains_key(need)) || self.lines.provided(need)
    }
    fn device(&mut self, _need: &str, _tag: lemnos_core::Tag) -> Result<lemnos_core::LocalCaller> {
        Err(Status::NOT_SUPPORTED)
    }
    fn i2c(&mut self, need: &str) -> Result<lemnos_core::I2c> {
        if !self.home.chain().with_world(|w| w.buses.contains_key(need)) {
            return Err(Status::NOT_FOUND);
        }
        Ok(lemnos_core::I2c(lemnos_core::Held::boxed(Box::new(
            Handle {
                bus: need.to_string(),
                own: HashSet::new(),
                home: self.home.clone(),
            },
        ))))
    }
    fn line(&mut self, need: &str) -> Result<lemnos_core::Line> {
        self.lines.line(need)
    }
}

impl Board {
    /// A backend over `lines` on `home`'s chain.
    #[must_use]
    pub fn on(home: Home, lines: SimBoard) -> Self {
        Self { lines, home }
    }
}

// ---------------------------------------------------------------- the receiver ("rp1-csi2" stand-in)
const RKEYS: &[Key] = &[
    Key::optional("device", Kind::Str),
    Key::optional("embedded_bytes", Kind::Int),
];

/// What a frame shows, when the receiver makes it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shows {
    /// The process's chain: the picture is the sensor's exposure level, or the shared scene when the
    /// test pattern is on (as it always was).
    Legacy,
    /// Paced: deterministic bytes of the frame's identity ([`scene::fill_paced`]).
    Paced,
    /// Lock-step: the instance's scene at the exposure and gain the frame was given.
    Scene,
}

#[driver("rp1-csi2", provides = [CameraReceiver])]
pub struct SimReceiver {
    /// The receiver driver's needs, one for one (`drivers/rp1-csi2`): a test compares the manifests.
    #[needs(optional, free)]
    frames: Option<StreamHandle>,
    #[link]
    _isp: Option<Dev<dyn CameraIsp>>,
    #[needs(optional, free)]
    embedded: Option<StreamHandle>,
    configured: Option<LinkConfig>,
    layout: Option<FrameLayout>,
    running: bool,
    counters: Counters,
    emitted: u64,
    latest_ns: u64,
    /// The shared scene as packed RAW10, made once, for a test pattern.
    scene: Option<Vec<u8>>,
    /// Which chain this receiver is on.
    home: Home,
    /// Paced by the clock: the next frame's start (ns on the clock the chain reads).
    real_next: Option<u64>,
    /// One frame's bytes, made once and reused by the paced and lock-step frames.
    scratch: Vec<u8>,
}

impl Lifecycle for SimReceiver {
    type Needs = SimReceiverNeeds;
    const TICKS: bool = true;
    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(RKEYS)
    }
    fn start(needs: SimReceiverNeeds, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            frames: needs.frames,
            _isp: needs._isp,
            embedded: needs.embedded,
            configured: None,
            layout: None,
            running: false,
            counters: Counters::default(),
            emitted: 0,
            latest_ns: 0,
            scene: None,
            home: Home::lookup(cfg.board()),
            real_next: None,
            scratch: Vec::new(),
        })
    }
    fn health(&self) -> Health {
        let mut faults = self.home.chain().faults();
        match faults.gone {
            Some(why) => {
                faults.gone_reports += 1;
                Health::Gone(why)
            }
            None => Health::Ok,
        }
    }
    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        profile::timed("sim-receiver", || self.step(now_ns))
    }
}

impl SimReceiver {
    /// How this receiver makes its frames now: the process's own chain follows the sensor's steps
    /// when a test puts them on it, and otherwise its pace.
    fn pace(&self) -> Pace {
        if !self.home.is_instance() && pisp::is_paced() {
            return Pace::Stepped;
        }
        self.home.chain().pace()
    }

    /// One tick of the receiver: the sensor's starts since the last tick become frames.
    fn step(&mut self, now_ns: u64) -> Option<u64> {
        if !self.running {
            return None;
        }
        let again = Some(now_ns + 1_000_000);
        let home = self.home.clone();
        let chain = home.chain();
        {
            let mut faults = chain.faults();
            if faults.consumer {
                faults.consumer = false;
                drop(faults);
                if let Some(frames) = self.frames.as_mut() {
                    let _ = frames.fault_consumer();
                }
                return again;
            }
        }
        if !chain.with_world(|w| w.streaming) {
            // A sensor step with the receiver not streaming makes no frame, but it is still taken.
            while chain.take_step().is_some() {
                chain.finish_step();
            }
            return again;
        }
        match self.pace() {
            Pace::Real => {
                self.tick_real(&home, now_ns);
                return again;
            }
            Pace::Stepped => {
                // Paced: one frame per sensor start the test steps, in the order they were stepped.
                while let Some(at) = chain.take_step() {
                    let shows = if home.is_instance() {
                        Shows::Paced
                    } else {
                        Shows::Legacy
                    };
                    self.start_frame(chain, at, false, shows);
                    chain.finish_step();
                }
            }
            Pace::Lockstep => {
                // One frame per grant: a grant waits for its buffer and is taken when its frame is made.
                while let Some(at) = chain.take_step() {
                    if !self.start_frame(chain, at, true, Shows::Scene) {
                        chain.untake_step(at);
                        break;
                    }
                    chain.finish_step();
                }
            }
            Pace::Demand => {
                // Demand: the next start is made when a buffer is free for it, and waits for one otherwise.
                let period = chain.period_ns();
                let at = chain.clock.peek_start(period);
                if self.start_frame(chain, at, true, Shows::Legacy) {
                    let _ = chain.clock.take_start(period);
                }
            }
        }
        again
    }

    /// Makes the frames that have fallen due on the clock since the last tick. The grid starts a
    /// period after the sensor's start (`World::started_at`), and runs on while the sensor streams:
    /// when it stops, the grid is forgotten, and a new start begins a new one. Each due frame is
    /// stamped with its own grid time and made if a buffer is free; with none it is dropped and
    /// counted. Nothing waits and nothing is queued past the ring.
    fn tick_real(&mut self, home: &Home, now_ns: u64) {
        let chain = home.chain();
        let (streaming, started) = chain.with_world(|w| (w.streaming, w.started_at));
        if !streaming {
            self.real_next = None;
            return;
        }
        let now = if chain.injected() {
            chain.clock.now()
        } else {
            now_ns
        };
        let period = chain.period_ns();
        let mut next = self.real_next.unwrap_or(started + period);
        while next <= now {
            self.start_frame(chain, next, false, Shows::Paced);
            next += period;
        }
        self.real_next = Some(next);
    }
}

impl Drop for SimReceiver {
    fn drop(&mut self) {
        self.halt();
    }
}

impl SimReceiver {
    /// Stops the receiver's running state, if it has one, and leaves the sensor steps' registry.
    fn halt(&mut self) {
        if self.running {
            self.running = false;
            self.home.chain().receivers(false);
        }
    }

    /// Makes the frame for the sensor start `at_ns` (sim ns) if a buffer is free. With `wait` a
    /// start with no buffer is left for later (`false` is returned); without it the start is
    /// dropped and counted. Returns whether the start was taken.
    ///
    /// A silent receiver takes every start and makes none, and keeps the first frame it did not make.
    fn start_frame(&mut self, chain: &Chain, at_ns: u64, wait: bool, shows: Shows) -> bool {
        let id = self.emitted;
        {
            let mut faults = chain.faults();
            if faults.silent {
                faults.first_unmade.get_or_insert(id);
                drop(faults);
                self.emitted += 1;
                self.latest_ns = at_ns;
                return true;
            }
        }
        let Some(frames) = self.frames.as_mut() else {
            self.counters.errors += 1;
            return true;
        };
        if shows != Shows::Paced {
            chain.clock.advance_to(at_ns);
        }
        let buffer = match frames.next_buffer() {
            Ok(Some(buffer)) => buffer,
            Ok(None) if !wait => {
                self.counters.dropped += 1;
                self.emitted += 1;
                self.latest_ns = at_ns;
                return true;
            }
            Ok(None) => return false,
            Err(_) => {
                self.counters.errors += 1;
                return true;
            }
        };
        let size = self.layout.map_or(0, |l| l.size);
        let meta = match shows {
            Shows::Legacy => {
                self.write_legacy(chain, buffer, size, at_ns);
                None
            }
            Shows::Paced => {
                if self.scratch.len() != size as usize {
                    self.scratch.resize(size as usize, 0);
                }
                scene::fill_paced(id, &mut self.scratch);
                self.write_scratch(buffer)
            }
            Shows::Scene => {
                let meta = lock_frame(chain, id, at_ns);
                if self.scratch.len() != size as usize {
                    self.scratch.resize(size as usize, 0);
                }
                match &*chain.scene() {
                    scene::Scene::Pattern => scene::render_pattern(
                        &mut self.scratch,
                        meta.exposure_us,
                        meta.analogue_gain_milli,
                    ),
                    scene::Scene::Replay(frames) => {
                        let frame = frames.get(id as usize % frames.len().max(1));
                        self.scratch.fill(0);
                        if let Some(frame) = frame {
                            let n = frame.len().min(self.scratch.len());
                            self.scratch[..n].copy_from_slice(&frame[..n]);
                        }
                    }
                }
                self.write_scratch(buffer);
                Some(meta)
            }
        };
        let frames = match self.frames.as_mut() {
            Some(frames) => frames,
            None => return true,
        };
        let frame = StreamFrame {
            buffer,
            bytes: size,
            sequence: id,
            identity: 0,
            timestamp_ns: at_ns.max(1),
            end_ns: 0,
        };
        match frames.publish(frame) {
            Ok(()) => self.counters.frames += 1,
            Err(_) => self.counters.errors += 1,
        }
        let _ = meta;
        self.emitted += 1;
        self.latest_ns = at_ns;
        true
    }

    /// The process's own chain's frame: the shared scene with the test pattern on, else the level.
    fn write_legacy(&mut self, chain: &Chain, buffer: u32, size: u32, at_ns: u64) {
        let pattern = chain.with_world(|w| {
            w.buses["i2c-10"].targets[&0x60]
                .regs
                .get(&lemnos_driver_ov9782::chip::TEST_PATTERN)
                .is_some_and(|v| v & 0x80 != 0)
        });
        let scene_frame = (pattern
            && size as usize == scene::RAW10_STRIDE * scene::HEIGHT as usize)
            .then(|| &*self.scene.get_or_insert_with(scene::reference_raw10));
        let period = chain.period_ns();
        let level = chain.with_world(|w| w.level_for_frame_at(at_ns, period));
        if let Some(frames) = self.frames.as_mut() {
            let wrote = match scene_frame {
                Some(picture) => frames.write(buffer, 0, picture),
                None => frames.write(buffer, 0, &vec![level; size as usize]),
            };
            if wrote.is_err() {
                self.counters.errors += 1;
            }
        }
    }

    /// Writes the scratch frame into `buffer`, returning nothing; a failed write is counted.
    fn write_scratch(&mut self, buffer: u32) -> Option<Meta> {
        if let Some(frames) = self.frames.as_mut()
            && frames.write(buffer, 0, &self.scratch).is_err()
        {
            self.counters.errors += 1;
        }
        None
    }
}

/// Makes frame `id`'s exposure and gain (the controls that land on it take effect first), and logs
/// the frame. The meta is what the frame was made with.
fn lock_frame(chain: &Chain, id: u64, at_ns: u64) -> Meta {
    let mut lock = chain.lock();
    let mut applied = [None, None];
    if let Some(list) = lock.pending.remove(&id) {
        for (slot, control) in applied.iter_mut().zip(list.iter()) {
            match *control {
                Control::Exposure(us) => lock.exposure_us = us,
                Control::Gain(milli) => lock.gain_milli = milli,
            }
            *slot = Some(*control);
        }
    }
    let meta = Meta {
        exposure_us: lock.exposure_us,
        analogue_gain_milli: lock.gain_milli,
        applied,
        sensor_timestamp_ns: at_ns,
    };
    lock.meta.insert(id, meta);
    while lock.meta.len() as u64 > chain::META_KEEP {
        let first = lock.meta.keys().next().copied();
        match first {
            Some(k) => {
                lock.meta.remove(&k);
            }
            None => break,
        }
    }
    meta
}

impl CameraReceiver for SimReceiver {
    fn limits(&mut self) -> Result<Limits> {
        Ok(Limits {
            max_lanes: 2,
            max_link_rate_mbps: 1500,
            max_width: 4096,
            max_height: 4096,
        })
    }
    fn configure(&mut self, link: LinkConfig, packing: Packing) -> Result<FrameLayout> {
        if self.running {
            return Err(Status::BUSY);
        }
        let line = match packing {
            Packing::Packed => (u64::from(link.width) * u64::from(link.bit_depth)).div_ceil(8),
            Packing::Unpacked16 => u64::from(link.width) * 2,
        };
        let stride = line.next_multiple_of(16);
        let layout = FrameLayout {
            width: link.width,
            height: link.height,
            bit_depth: link.bit_depth,
            packing,
            stride: u32::try_from(stride).map_err(|_| Status::INVALID_ARGUMENT)?,
            size: u32::try_from(stride * u64::from(link.height))
                .map_err(|_| Status::INVALID_ARGUMENT)?,
        };
        self.configured = Some(link);
        self.layout = Some(layout);
        Ok(layout)
    }
    fn link(&mut self) -> Result<Option<LinkConfig>> {
        Ok(self.configured)
    }
    fn frame_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        let name = b"frames";
        debug_assert!(name.len() <= MAX_STREAM_NAME);
        into.get_mut(..name.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(name);
        Ok(name.len())
    }
    fn start(&mut self) -> Result<()> {
        self.configured.ok_or(Status::INVALID_ARGUMENT)?;
        if self.running {
            return Err(Status::BUSY);
        }
        let chain = self.home.chain();
        {
            // A refused start: the next N starts fail with `IO`, and the host tries again.
            let mut faults = chain.faults();
            if faults.refusals > 0 {
                faults.refusals -= 1;
                return Err(Status::IO);
            }
        }
        // The module's rule (see `EXCLUSIVE`): no raw stream while the front end streams.
        chain.take(RAW)?;
        self.counters = Counters::default();
        self.emitted = 0;
        chain.clock.reset_schedule();
        self.running = true;
        self.real_next = None;
        chain.receivers(true);
        Ok(())
    }
    fn stop(&mut self) -> Result<()> {
        self.halt();
        self.home.chain().release(RAW);
        Ok(())
    }
    fn counters(&mut self) -> Result<Counters> {
        Ok(self.counters)
    }
    fn feeds(&mut self, _into: &mut [u8]) -> Result<usize> {
        Ok(0)
    }
    fn embedded_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        let _ = &self.embedded;
        Ok(0)
    }
    /// As `drivers/rp1-csi2/src/lib.rs:679`: the module knows the block, not which sensor feeds it.
    fn source(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }
    fn layout(&mut self) -> Result<Option<FrameLayout>> {
        Ok(self.layout)
    }
    fn frame_starts(&mut self) -> Result<Option<FrameStarts>> {
        // None only while nothing streams (the module's rule): a started receiver counts from 0.
        if !self.running {
            return Ok(None);
        }
        Ok(Some(FrameStarts {
            count: self.emitted,
            latest_ns: self.latest_ns,
        }))
    }
    fn inputs(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }
    fn input(&mut self) -> Result<u32> {
        Err(Status::NOT_SUPPORTED)
    }
}

// ---------------------------------------------------------------- the daemon's parts
/// Every driver the simulated board runs: the real sensor, power switch and raw bus drivers, the
/// receiver stand-in, and the real PiSP drivers.
#[must_use]
pub fn drivers() -> Vec<DriverEntry> {
    vec![
        entry::<lemnos_driver_ov9782::Ov9782>(),
        entry::<lemnos_driver_raw_bus::RawBus>(),
        entry::<lemnos_driver_gpio_power_switch::GpioPowerSwitch>(),
        entry::<SimReceiver>(),
        entry::<lemnos_driver_pisp::PispFe>(),
        entry::<lemnos_driver_pisp::PispBe>(),
    ]
}

/// The drivers an instance runs: the sensor, power switch, raw bus and receiver (no PiSP blocks).
#[must_use]
pub fn instance_drivers() -> Vec<DriverEntry> {
    vec![
        entry::<lemnos_driver_ov9782::Ov9782>(),
        entry::<lemnos_driver_raw_bus::RawBus>(),
        entry::<lemnos_driver_gpio_power_switch::GpioPowerSwitch>(),
        entry::<SimReceiver>(),
    ]
}

/// Where the buffers of a stream of dma-bufs come from: this host's `system` dma-heap when it can
/// be opened (real dma-bufs, which cannot be sealed and cannot be read or written with `pread`:
/// what the board's heap gives), and otherwise sealed memfds, which can. [`SimBuffers::kind`]
/// says which.
pub struct SimBuffers {
    heaps: Option<lemnosd::DmaHeaps>,
}

impl SimBuffers {
    /// Probes the host's heap once: one buffer is made and dropped. With `LEMNOS_TEST_NO_DMA_HEAP`
    /// set (test only) the heap is treated as absent, so the buffers are sealed memfds whatever the
    /// host has: the kind a CI runner without a dma-heap gets. Nothing else is changed by it.
    #[must_use]
    pub fn probe() -> Self {
        if std::env::var_os("LEMNOS_TEST_NO_DMA_HEAP").is_some() {
            return Self { heaps: None };
        }
        let heaps = lemnosd::DmaHeaps::at("/dev/dma_heap");
        let usable = lemnosd::BufferSource::allocate(&heaps, "system", 4096, 1).is_ok();
        Self {
            heaps: usable.then_some(heaps),
        }
    }

    /// Sealed memfds, whatever the host has.
    #[must_use]
    pub fn memfd() -> Self {
        Self { heaps: None }
    }

    /// The host's dma-heap, or `None` when it has none that works.
    #[must_use]
    pub fn dma_heap() -> Option<Self> {
        let heaps = lemnosd::DmaHeaps::at("/dev/dma_heap");
        let usable = lemnosd::BufferSource::allocate(&heaps, "system", 4096, 1).is_ok();
        usable.then_some(Self { heaps: Some(heaps) })
    }

    /// What the buffers are.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        if self.heaps.is_some() {
            "dma-bufs from the host's system heap"
        } else {
            "sealed memfds (the host has no usable dma-heap)"
        }
    }
}

impl lemnosd::BufferSource for SimBuffers {
    fn allocate(
        &self,
        _heap: &str,
        bytes: u64,
        count: u32,
    ) -> std::io::Result<Vec<std::os::fd::OwnedFd>> {
        match &self.heaps {
            Some(heaps) => lemnosd::BufferSource::allocate(heaps, "system", bytes, count),
            None => (0..count)
                .map(|_| lemnos_linux_sys::dma_buf::sealed_memfd(c"camera-sim-buffer", bytes))
                .collect(),
        }
    }
}

/// The camera chain of a reference board file: its `cam0-*` devices, the hardware and line they
/// use, and nothing else. Devices are listed after the devices their `link` names (the host orders
/// binds by `uses` only, so a device whose link is not yet running is refused its first bind).
///
/// # Errors
/// The text is not a board file.
pub fn chain_board(reference: &str) -> std::result::Result<lemnos_host::board::BoardFile, String> {
    let mut board = lemnos_host::board::parse_lenient(reference).map_err(|e| e.to_string())?;
    board.security = None;
    board.devices.retain(|d| d.id.starts_with("cam0-"));
    let rank = |id: &str| match id {
        "cam0-power" => 0,
        "cam0-isp" => 1,
        "cam0-fe" => 2,
        "cam0-receiver" => 3,
        _ => 4,
    };
    board.devices.sort_by_key(|d| rank(&d.id));
    board.hardware.retain(|h| {
        h.name.as_deref() == Some("i2c-10") || matches!(h.controller.as_str(), "csi0" | "pisp-be")
    });
    board.lines.retain(|l| l.line == "gpio:pinctrl-rp1:34");
    Ok(board)
}

/// A simulated board's [`Provider`] on the process's own chain: the sensor's bus and the power
/// switch's line.
#[must_use]
pub fn provider() -> Board {
    let lines = SimBoard::new();
    lines.add_line("gpio:pinctrl-rp1:34");
    Board::on(Home::Legacy, lines)
}
