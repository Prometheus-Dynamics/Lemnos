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

pub mod pisp;
pub mod scene;
pub mod time;

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, PoisonError};

use lemnos_contract_camera_isp::CameraIsp;
use lemnos_contract_camera_receiver::{
    CameraReceiver, Counters, FrameLayout, FrameStarts, Limits, LinkConfig, MAX_STREAM_NAME,
    Packing,
};
use lemnos_core::{
    Config, ConfigError, Dev, Key, Kind, Lifecycle, Provider, Result, Status, StreamHandle, driver,
};
use lemnos_hal::ErrorKind;
use lemnos_hal::blocks::StreamFrame;
use lemnos_hal::i2c::{ErrorType, I2c as HalI2c, Operation};
use lemnos_host::{DriverEntry, entry};
use lemnos_sim::SimBoard;

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

pub static WORLD: Mutex<Option<World>> = Mutex::new(None);

/// The module's exclusion, as `kernel/lemnos_rp1_csi2.c` and `kernel/lemnos_pisp_fe.c` make it: the
/// receiver's raw `frames` stream and the front end are never running together (the receiver's
/// `START` is refused with `EBUSY` while the front end streams, the front end's while a raw stream
/// runs), because in front-end mode the CSI-2 channel feeds the front end instead of memory. The
/// module takes the decision under one lock; here it is one compare-and-swap, so the later of two
/// starts always sees the earlier. A process holds one simulated chain, so this is process-wide;
/// [`new_world`] clears it.
pub static EXCLUSIVE: AtomicU8 = AtomicU8::new(NOBODY);
/// [`EXCLUSIVE`]: nothing streams.
pub const NOBODY: u8 = 0;
/// [`EXCLUSIVE`]: the receiver's raw stream runs.
pub const RAW: u8 = 1;
/// [`EXCLUSIVE`]: the front end has been started with a job.
pub const FRONT_END: u8 = 2;

/// Takes the block for `who`, `Err` (`EBUSY`) if the other one has it. Taking it again is fine for
/// the front end (a restart); for the raw stream the receiver's own `running` check refuses it.
pub fn take(who: u8) -> Result<()> {
    match EXCLUSIVE.compare_exchange(NOBODY, who, Ordering::SeqCst, Ordering::SeqCst) {
        Ok(_) => Ok(()),
        Err(held) if held == who && who == FRONT_END => Ok(()),
        Err(_) => Err(Status::BUSY),
    }
}

/// Lets go of the block if `who` holds it.
pub fn release(who: u8) {
    let _ = EXCLUSIVE.compare_exchange(who, NOBODY, Ordering::SeqCst, Ordering::SeqCst);
}

pub fn world<R>(f: impl FnOnce(&mut World) -> R) -> R {
    let mut guard = WORLD.lock().unwrap_or_else(PoisonError::into_inner);
    f(guard.as_mut().expect("world"))
}

fn put(regs: &mut BTreeMap<u16, u8>, at: u16, bytes: &[u8]) {
    for (i, b) in bytes.iter().enumerate() {
        regs.insert(at + i as u16, *b);
    }
}

pub fn new_world(claims_on: bool, model: Model) -> World {
    EXCLUSIVE.store(NOBODY, Ordering::SeqCst);
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

/// The sensor's next start is taken by every consumer of a sensor step ([`sensor_step`]): the front
/// ends by a call, each running receiver through this queue. The receiver's tick takes the starts
/// and says when each is made, so a step returns only once the receiver has made its frame.
struct Steps {
    pending: std::collections::VecDeque<u64>,
    /// Steps asked for, and steps the receiver has made.
    issued: u64,
    done: u64,
    /// Receivers running in this process.
    receivers: usize,
}

static STEPS: (Mutex<Steps>, std::sync::Condvar) = (
    Mutex::new(Steps {
        pending: std::collections::VecDeque::new(),
        issued: 0,
        done: 0,
        receivers: 0,
    }),
    std::sync::Condvar::new(),
);

/// How long a sensor step waits for a receiver's tick before it is named as stuck.
const STEP_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

fn steps() -> std::sync::MutexGuard<'static, Steps> {
    STEPS.0.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A receiver started (`true`) or stopped (`false`) in this process.
fn receivers(started: bool) {
    let mut s = steps();
    if started {
        s.receivers += 1;
    } else {
        s.receivers = s.receivers.saturating_sub(1);
        // Starts it would have made are not waited for any more.
        s.pending.clear();
        s.done = s.issued;
        STEPS.1.notify_all();
    }
}

/// The receiver's next step to make, if one was asked for.
fn take_step() -> Option<u64> {
    steps().pending.pop_front()
}

/// The receiver has made the step it took.
fn finish_step() {
    steps().done += 1;
    STEPS.1.notify_all();
}

/// One sensor start, shared by every consumer of the sensor. The clock moves to the start; each
/// front end with a job queued makes its frame, and each with none drops the start; each running
/// receiver makes its frame (or drops it, with no buffer). Returns once every one has done so.
///
/// # Panics
/// A running receiver does not take the step within ten seconds: its daemon's tick is not running.
pub fn sensor_step() -> u64 {
    let start = time::take_start(period_ns());
    time::advance_to(start);
    pisp::step_front_ends(start);
    let mut s = steps();
    if s.receivers == 0 {
        return start;
    }
    s.issued += 1;
    let wanted = s.issued;
    s.pending.push_back(start);
    STEPS.1.notify_all();
    let deadline = std::time::Instant::now() + STEP_WAIT;
    while s.done < wanted {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(
            !left.is_zero(),
            "sensor start {start} was not taken by the receiver within {STEP_WAIT:?}: its tick is not running"
        );
        s = STEPS
            .1
            .wait_timeout(s, left)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
    start
}

/// The frame period in ns, from the model's registers (at least 1).
#[must_use]
pub fn period_ns() -> u64 {
    u64::try_from(period_ps() / 1000).unwrap_or(u64::MAX).max(1)
}

/// The frame period in ps from the model's registers.
pub fn period_ps() -> u128 {
    world(|w| {
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
    })
}

// ---------------------------------------------------------------- the bus handle
/// One device's handle on a bus. With `--claims on` it behaves as `ModuleI2c`: the first transfer to an
/// address claims it, a second handle's transfer to a claimed address is `Busy`, an address that does not
/// answer is let go, the claims go with the handle.
struct Handle {
    bus: String,
    own: HashSet<u8>,
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
        world(|w| {
            w.transactions += 1;
            if w.claims_on && !self.own.contains(&address) {
                if !w.claims.insert((self.bus.clone(), address)) {
                    return Err(ErrorKind::Busy);
                }
                self.own.insert(address);
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
                w.launches.push((time::now(), units));
            }
            if address == 0x60 && self.bus == "i2c-10" {
                if let Some(v) = mode_select {
                    w.streaming = v & 1 == 1;
                } else {
                    w.streaming = streaming_before;
                }
            }
            Ok(())
        })
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        world(|w| {
            for a in self.own.drain() {
                w.claims.remove(&(self.bus.clone(), a));
            }
        });
    }
}

// ---------------------------------------------------------------- the backend
pub struct Board {
    pub lines: SimBoard,
}

impl Provider for Board {
    fn provided(&self, need: &str) -> bool {
        world(|w| w.buses.contains_key(need)) || self.lines.provided(need)
    }
    fn device(&mut self, _need: &str, _tag: lemnos_core::Tag) -> Result<lemnos_core::LocalCaller> {
        Err(Status::NOT_SUPPORTED)
    }
    fn i2c(&mut self, need: &str) -> Result<lemnos_core::I2c> {
        if !world(|w| w.buses.contains_key(need)) {
            return Err(Status::NOT_FOUND);
        }
        Ok(lemnos_core::I2c(lemnos_core::Held::boxed(Box::new(
            Handle {
                bus: need.to_string(),
                own: HashSet::new(),
            },
        ))))
    }
    fn line(&mut self, need: &str) -> Result<lemnos_core::Line> {
        self.lines.line(need)
    }
}

// ---------------------------------------------------------------- the receiver ("rp1-csi2" stand-in)
const RKEYS: &[Key] = &[
    Key::optional("device", Kind::Str),
    Key::optional("embedded_bytes", Kind::Int),
];

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
}

impl Lifecycle for SimReceiver {
    type Needs = SimReceiverNeeds;
    const TICKS: bool = true;
    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(RKEYS)
    }
    fn start(needs: SimReceiverNeeds, _cfg: &Config<'_>) -> Result<Self> {
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
        })
    }
    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        if !self.running {
            return None;
        }
        let again = Some(now_ns + 1_000_000);
        if !world(|w| w.streaming) {
            // A sensor step with the receiver not streaming makes no frame, but it is still taken.
            while let Some(_at) = take_step() {
                finish_step();
            }
            return again;
        }
        if pisp::is_paced() {
            // Paced: one frame per sensor start the test steps, in the order they were stepped.
            while let Some(at) = take_step() {
                self.start_frame(at, true);
                finish_step();
            }
            return again;
        }
        // Demand: the next start is made when a buffer is free for it, and waits for one otherwise.
        let period = period_ns();
        let at = time::peek_start(period);
        if self.start_frame(at, false) {
            let _ = time::take_start(period);
        }
        again
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
            receivers(false);
        }
    }

    /// Makes the frame for the sensor start `at` (sim ns) if a buffer is free. Paced, a start with
    /// no buffer is dropped and counted; demand, it is left for the next tick. Returns whether the
    /// start was taken.
    fn start_frame(&mut self, at_ns: u64, paced: bool) -> bool {
        let Some(frames) = self.frames.as_mut() else {
            self.counters.errors += 1;
            return true;
        };
        time::advance_to(at_ns);
        let buffer = match frames.next_buffer() {
            Ok(Some(buffer)) => buffer,
            Ok(None) if paced => {
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
        // A test pattern is a static picture the sensor makes itself: with the pattern on
        // (`0x5e00` bit 7) the frames are the shared scene as packed RAW10, the same picture the
        // simulated front end writes, so a frame saved from the receiver is a reference for the
        // front end's. With the pattern off the picture is the exposure's level, as before.
        let pattern = world(|w| {
            w.buses["i2c-10"].targets[&0x60]
                .regs
                .get(&lemnos_driver_ov9782::chip::TEST_PATTERN)
                .is_some_and(|v| v & 0x80 != 0)
        });
        let scene_frame = (pattern
            && size as usize == scene::RAW10_STRIDE * scene::HEIGHT as usize)
            .then(|| &*self.scene.get_or_insert_with(scene::reference_raw10));
        // The picture's brightness is the sensor's exposure as it latched (`World::level_for_frame_at`).
        let period = period_ns();
        let level = world(|w| w.level_for_frame_at(at_ns, period));
        let Some(frames) = self.frames.as_mut() else {
            return true;
        };
        let wrote = match scene_frame {
            Some(picture) => frames.write(buffer, 0, picture),
            None => frames.write(buffer, 0, &vec![level; size as usize]),
        };
        if wrote.is_err() {
            self.counters.errors += 1;
        }
        let frame = StreamFrame {
            buffer,
            bytes: size,
            sequence: self.emitted,
            identity: 0,
            timestamp_ns: at_ns,
            end_ns: 0,
        };
        match frames.publish(frame) {
            Ok(()) => self.counters.frames += 1,
            Err(_) => self.counters.errors += 1,
        }
        self.emitted += 1;
        self.latest_ns = at_ns;
        true
    }
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
        // The module's rule (see `EXCLUSIVE`): no raw stream while the front end streams.
        take(RAW)?;
        self.counters = Counters::default();
        self.emitted = 0;
        time::reset_schedule();
        self.running = true;
        receivers(true);
        Ok(())
    }
    fn stop(&mut self) -> Result<()> {
        self.halt();
        release(RAW);
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
/// Every driver the simulated board runs: the real sensor, power switch, raw bus and PiSP
/// drivers, and the receiver stand-in.
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

/// A simulated board's [`Provider`]: the sensor's bus and the power switch's line.
#[must_use]
pub fn provider() -> Board {
    let lines = SimBoard::new();
    lines.add_line("gpio:pinctrl-rp1:34");
    Board { lines }
}
