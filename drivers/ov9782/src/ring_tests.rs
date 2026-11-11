//! Values from the ring against a simulated frame clock.
//!
//! The bus takes `TXN_NS` for each transaction. Frame `k` starts `k` frame periods after frame 0,
//! where frame 0 is the moment the sensor starts streaming. The period is the mode's own: 1822 lines
//! of 9562.5 ns. A group whose launch write completes inside frame `F` is latched at the start of
//! frame `F + 1` and shows `Mode::exposure_delay` frames after `F`, so a group completing after
//! that start shows one frame later. The sensor's group hold is taken to behave that way; a board
//! run must confirm it (`testing/board-runs/D10.md`).
//!
//! The consumer posts at any time. The driver reads the ring on its own poll, every
//! `VALUES_POLL_NS` from the first tick after start, so a post is read at the first poll after it.
//! Every expected frame number below is written out by hand.

use super::*;
use chip::{
    CHIP_ID, DEFAULT_ADDRESS, DEFAULT_EXPOSURE_LINES, EXPOSURE, EXPOSURE_FRACTION_BITS,
    GROUP_ACCESS, GROUP_HOLD_END, GROUP_HOLD_START, GROUP_LAUNCH, MODE_SELECT, STREAMING,
};
use lemnos_core::{Driver, Encode, Writer};
use lemnos_hal::ErrorKind;
use lemnos_hal::StreamEndpoint;
use lemnos_hal::blocks::{BufferHandle, BufferKind, StreamFrame};
use lemnos_hal::i2c::{ErrorType, I2c as HalI2c, Operation};
use lemnos_hal::mock::MockI2c;
use lemnos_hal::register::AddressWidth;
use lemnos_wire::ring::{Consumer, Layout, Producer, heap_words};

extern crate alloc;
extern crate std;
use alloc::boxed::Box;
use alloc::vec::Vec;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const ADDR: u8 = DEFAULT_ADDRESS;
/// Nanoseconds per I2C transaction.
const TXN_NS: u64 = 50_000;
/// 1822 lines (800 active plus the default 1022 blanking) at 9562.5 ns.
const PERIOD_NS: u64 = 17_422_875;
const POLL_NS: u64 = VALUES_POLL_NS;
const MS: u64 = 1_000_000;
/// Exposure after start, in sixteenths of a line.
const BASE_EXPOSURE: u32 = DEFAULT_EXPOSURE_LINES << EXPOSURE_FRACTION_BITS;
const EXPOSURE_REGS: [u16; 3] = [EXPOSURE, EXPOSURE + 1, EXPOSURE + 2];
/// Exposure for 100 lines, and for 150 lines.
const SHORT: u32 = 1600;
const LONG: u32 = 2400;

/// One transaction as the bus saw it: it started at `at` and completed at `done`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Txn {
    at: u64,
    done: u64,
    register: u16,
    /// The data byte of a three-byte register write.
    value: Option<u8>,
    ok: bool,
}

/// Each transaction takes `TXN_NS` of the shared clock, and the Nth from now can fail.
#[derive(Clone)]
struct Bus {
    mock: MockI2c,
    clock: Arc<AtomicU64>,
    log: Arc<Mutex<Vec<Txn>>>,
    fail_at: Arc<AtomicI64>,
}

impl ErrorType for Bus {
    type Error = ErrorKind;
}

impl HalI2c for Bus {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> core::result::Result<(), ErrorKind> {
        let at = self.clock.fetch_add(TXN_NS, Ordering::SeqCst);
        let (register, value) = match operations.first() {
            Some(Operation::Write(bytes)) => (
                u16::from_be_bytes([bytes[0], bytes[1]]),
                (bytes.len() == 3).then(|| bytes[2]),
            ),
            _ => (0, None),
        };
        let fail = match self.fail_at.load(Ordering::SeqCst) {
            0 => false,
            left => {
                self.fail_at.store(left - 1, Ordering::SeqCst);
                left == 1
            }
        };
        self.log.lock().unwrap().push(Txn {
            at,
            done: at + TXN_NS,
            register,
            value,
            ok: !fail,
        });
        if fail {
            return Err(ErrorKind::Nack);
        }
        self.mock.transaction(address, operations)
    }
}

/// The consumer's side of the ring is `Consumer`. The driver's side is this: the host's own
/// endpoint is a thin wrapper over the same `Producer`, and the host tests cover that.
struct RingEnd(Producer);

impl StreamEndpoint for RingEnd {
    type Error = ErrorKind;

    fn buffer_count(&self) -> u32 {
        0
    }

    fn buffer_kind(&self) -> BufferKind {
        BufferKind::Plain
    }

    fn publish(&mut self, _frame: StreamFrame) -> core::result::Result<(), ErrorKind> {
        Err(ErrorKind::Unsupported)
    }

    fn next_buffer(&mut self) -> core::result::Result<Option<u32>, ErrorKind> {
        Ok(None)
    }

    fn next_job(&mut self, out: &mut [u8]) -> core::result::Result<Option<usize>, ErrorKind> {
        self.0.pop_job(out).map_err(|_| ErrorKind::InvalidInput)
    }

    fn write(
        &mut self,
        _buffer: u32,
        _offset: usize,
        _data: &[u8],
    ) -> core::result::Result<bool, ErrorKind> {
        Err(ErrorKind::Unsupported)
    }

    fn read(
        &mut self,
        _buffer: u32,
        _offset: usize,
        _out: &mut [u8],
    ) -> core::result::Result<bool, ErrorKind> {
        Err(ErrorKind::Unsupported)
    }

    fn buffer_handle(
        &self,
        _buffer: u32,
    ) -> core::result::Result<Option<BufferHandle<'_>>, ErrorKind> {
        Err(ErrorKind::Unsupported)
    }

    fn fault_consumer(&mut self) -> core::result::Result<(), ErrorKind> {
        Ok(())
    }

    fn buffer_current(&self, _buffer: u32) -> core::result::Result<bool, ErrorKind> {
        Ok(true)
    }
}

/// A group the sensor would latch: the launch that closed it, and its exposure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Launch {
    /// The start of the transaction that opened the group: when the driver read the job.
    issued: u64,
    /// When the launch write completed.
    done: u64,
    exposure: u32,
}

#[derive(Default)]
struct Group {
    at: u64,
    exposure: [Option<u8>; 3],
}

/// The launches in `log`, and the exposure writes that were not inside a group hold, from `from`.
fn launches(log: &[Txn], from: u64) -> (Vec<Launch>, usize) {
    let mut out = Vec::new();
    let mut open: Option<Group> = None;
    let mut closed: Option<Group> = None;
    let mut loose = 0;
    for t in log.iter().filter(|t| t.ok) {
        let Some(value) = t.value else { continue };
        match t.register {
            GROUP_ACCESS if value == GROUP_HOLD_START => {
                open = Some(Group {
                    at: t.at,
                    ..Group::default()
                })
            }
            GROUP_ACCESS if value == GROUP_HOLD_END => closed = open.take(),
            GROUP_ACCESS if value == GROUP_LAUNCH => {
                if let Some(group) = closed.take() {
                    let [a, b, c] = group.exposure;
                    if let (Some(a), Some(b), Some(c)) = (a, b, c) {
                        let exposure = u32::from(a) << 16 | u32::from(b) << 8 | u32::from(c);
                        out.push(Launch {
                            issued: group.at,
                            done: t.done,
                            exposure,
                        });
                    }
                }
            }
            register if EXPOSURE_REGS.contains(&register) && t.at >= from => match open.as_mut() {
                Some(group) => group.exposure[usize::from(register - EXPOSURE)] = Some(value),
                None => loose += 1,
            },
            _ => {}
        }
    }
    (out, loose)
}

/// Frame `frame_of(t)` is the frame `t` falls in.
fn frame_of(t: u64, frame0: u64) -> u64 {
    (t - frame0) / PERIOD_NS
}

/// The exposure `frame` shows: the last launch that shows by then, else the start value.
fn shows(launched: &[Launch], frame0: u64, delay: u64, frame: u64) -> u32 {
    launched
        .iter()
        .rfind(|l| frame_of(l.done, frame0) + delay <= frame)
        .map_or(BASE_EXPOSURE, |l| l.exposure)
}

struct Rig {
    bus: Bus,
    clock: Arc<AtomicU64>,
    log: Arc<Mutex<Vec<Txn>>>,
    sensor: Ov9782,
    jobs: Consumer,
    /// The start of frame 0: when the start's last register write completed.
    frame0: u64,
    /// When the driver next wants a tick.
    next: Option<u64>,
}

impl Rig {
    /// A streaming sensor, mode 0, with a 64-byte job ring.
    fn new() -> Self {
        let clock = Arc::new(AtomicU64::new(0));
        let log = Arc::new(Mutex::new(Vec::new()));
        let mock = MockI2c::new()
            .with_target(ADDR, AddressWidth::Bits16)
            .with_registers(ADDR, CHIP_ID, &0x9782u16.to_be_bytes());
        let bus = Bus {
            mock,
            clock: Arc::clone(&clock),
            log: Arc::clone(&log),
            fail_at: Arc::new(AtomicI64::new(0)),
        };
        let layout = Layout::new(1, 64, 8, 64).unwrap();
        let memory = heap_words(layout.ring_words());
        let producer = Producer::new(memory.clone(), layout).unwrap();
        let jobs = Consumer::attach(memory, layout).unwrap();
        let no_config: &[(&str, ConfigValue<'_>)] = &[];
        let mut sensor = <Ov9782 as Lifecycle>::start(
            Ov9782Needs {
                bus: I2c(Box::new(bus.clone())),
                values: Some(Box::new(RingEnd(producer))),
            },
            &Config::new(no_config),
        )
        .unwrap();
        sensor.select_mode(0).unwrap();
        sensor.start().unwrap();
        let frame0 = log
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|t| t.register == MODE_SELECT && t.value == Some(STREAMING))
            .map(|t| t.done)
            .unwrap();
        let mut rig = Self {
            bus,
            clock,
            log,
            sensor,
            jobs,
            frame0,
            next: None,
        };
        rig.next = Lifecycle::tick(&mut rig.sensor, frame0);
        rig
    }

    /// The time `frame` plus `offset` into it.
    fn at(&self, frame: u64, offset: u64) -> u64 {
        self.frame0 + frame * PERIOD_NS + offset
    }

    /// The driver's poll grid: `ms` milliseconds after frame 0. Frame starts do not fall on it.
    fn grid(&self, ms: u64) -> u64 {
        self.frame0 + ms * MS
    }

    /// Runs the driver's own ticks up to `until`, as the host does: each tick at the time the
    /// previous one returned, or later if the bus is still busy from the one before.
    fn run_to(&mut self, until: u64) {
        while let Some(due) = self.next.filter(|due| *due <= until) {
            let now = self.clock.load(Ordering::SeqCst).max(due);
            self.clock.store(now, Ordering::SeqCst);
            self.next = Lifecycle::tick(&mut self.sensor, now);
        }
    }

    /// The consumer posts `FrameValues` with an exposure, at time `at`.
    fn post(&mut self, at: u64, identity: u64, exposure: u32) {
        self.run_to(at);
        let mut buf = [0u8; 64];
        let mut out = Writer::new(&mut buf);
        FrameValues {
            identity,
            controls: Controls {
                exposure: Some(exposure),
                ..Controls::default()
            },
        }
        .encode(&mut out)
        .unwrap();
        let len = out.len();
        assert!(self.jobs.push_job(&buf[..len]).unwrap());
    }

    fn launched(&self) -> (Vec<Launch>, usize) {
        let log = self.log.lock().unwrap();
        launches(&log, self.frame0)
    }

    fn shows(&self, frame: u64) -> u32 {
        let (launched, _) = self.launched();
        shows(&launched, self.frame0, DELAY, frame)
    }

    /// The register writes at or after `from`, as (register, value, ok).
    fn writes_from(&self, from: u64) -> Vec<(u16, Option<u8>, bool)> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.at >= from)
            .map(|t| (t.register, t.value, t.ok))
            .collect()
    }
}

/// `Mode::exposure_delay` for mode 0: checked against the table in `the_frame_clock_*`.
const DELAY: u64 = 2;

/// The sensor starts with its values, at the mode's own frame clock.
#[test]
fn the_frame_clock_is_the_mode_table_and_a_job_fits_the_ring() {
    let mut rig = Rig::new();
    let mode = rig.sensor.mode(0).unwrap();
    assert_eq!(mode.line_time_ps, 9_562_500);
    assert_eq!(chip::MODES[0].frame_length_default(), 1822);
    assert_eq!(PERIOD_NS, 1822 * 9_562_500 / 1000);
    assert_eq!(u64::from(mode.exposure_delay), DELAY);
    // The first poll is one poll after the start.
    assert_eq!(rig.next, Some(rig.frame0 + POLL_NS));
    // The largest job has every control set, and still fits the 64-byte job ring.
    let mut buf = [0u8; 64];
    let mut out = Writer::new(&mut buf);
    FrameValues {
        identity: u64::MAX,
        controls: Controls {
            exposure: Some(u32::MAX),
            analogue_gain: Some(u32::MAX),
            digital_gain: Some(u32::MAX),
            frame_length: Some(u32::MAX),
        },
    }
    .encode(&mut out)
    .unwrap();
    assert!(out.len() <= 64);
    rig.run_to(rig.at(1, 0));
    assert_eq!(rig.shows(0), BASE_EXPOSURE);
}

/// Posted 7.5 ms into frame 5 and read by the 95 ms poll, which is still in frame 5 (frame 5 runs
/// from 87.1 ms to 104.5 ms after frame 0): the group launches in frame 5 and shows on frame 7.
#[test]
fn a_post_early_in_a_frame_lands_two_frames_later() {
    let mut rig = Rig::new();
    rig.post(rig.at(5, 7 * MS + MS / 2), 1, SHORT);
    rig.run_to(rig.at(9, 0));
    let (launched, loose) = rig.launched();
    assert_eq!(loose, 0, "every exposure write is inside a group hold");
    assert_eq!(launched.len(), 1);
    assert_eq!(launched[0].issued, rig.grid(95));
    assert_eq!(frame_of(launched[0].done, rig.frame0), 5);
    assert_eq!(launched[0].exposure, SHORT);
    assert_eq!(rig.shows(6), BASE_EXPOSURE);
    assert_eq!(rig.shows(7), SHORT);
    assert_eq!(rig.applied_identity(), Some(1));
}

/// The deadline. Frame 6 starts 104.54 ms after frame 0. The last poll before that is at 104 ms,
/// and its six transactions complete at 104.3 ms, so the group completes in frame 5 and lands on
/// frame 7. A post at 103.6 ms is read by that poll.
#[test]
fn a_group_completing_before_the_next_frame_start_lands_two_frames_after_its_frame() {
    let mut rig = Rig::new();
    rig.post(rig.at(5, 16 * MS + MS / 2), 1, SHORT);
    rig.run_to(rig.at(9, 0));
    let (launched, _) = rig.launched();
    assert_eq!(launched.len(), 1);
    assert_eq!(launched[0].issued, rig.grid(104));
    assert_eq!(launched[0].done, rig.grid(104) + 6 * TXN_NS);
    assert!(
        launched[0].done < rig.at(6, 0),
        "completes before frame 6 starts"
    );
    assert_eq!(rig.shows(6), BASE_EXPOSURE);
    assert_eq!(rig.shows(7), SHORT);
}

/// Posted in frame 5 just after the 104 ms poll, the last that fits: the next poll is at 105 ms, in
/// frame 6, and its group completes after frame 6 starts. It lands on frame 8, three frames after
/// frame 5.
#[test]
fn a_late_post_lands_one_frame_later() {
    let mut rig = Rig::new();
    rig.post(rig.at(5, 17 * MS), 1, SHORT);
    rig.run_to(rig.at(9, 0));
    let (launched, _) = rig.launched();
    assert_eq!(launched.len(), 1);
    assert_eq!(launched[0].issued, rig.grid(105));
    assert_eq!(frame_of(launched[0].issued, rig.frame0), 6);
    assert_eq!(frame_of(launched[0].done, rig.frame0), 6);
    assert_eq!(rig.shows(7), BASE_EXPOSURE);
    assert_eq!(rig.shows(8), SHORT);
}

/// Two posts read by the same poll: the later one is applied, and the earlier shows on no frame.
#[test]
fn two_posts_in_one_poll_the_later_one_wins() {
    let mut rig = Rig::new();
    rig.post(rig.at(5, MS + 200_000), 1, SHORT);
    rig.post(rig.at(5, MS + 500_000), 2, LONG);
    rig.run_to(rig.at(12, 0));
    let (launched, _) = rig.launched();
    assert_eq!(launched.len(), 1, "one group for the poll");
    assert_eq!(launched[0].exposure, LONG);
    assert_eq!(rig.applied_identity(), Some(2));
    for frame in 0..12 {
        assert_ne!(
            rig.shows(frame),
            SHORT,
            "frame {frame} shows the earlier post"
        );
    }
    assert_eq!(rig.shows(7), LONG);
}

/// Two posts in one frame read by different polls: both groups complete in frame 5, so both
/// launch for the same frame start. The sensor latches the later group, so the later one shows on
/// frame 7 and the earlier shows on no frame. This is the sensor assumption a board run checks.
#[test]
fn two_posts_in_one_frame_in_different_polls_the_later_one_is_latched() {
    let mut rig = Rig::new();
    rig.post(rig.at(5, MS + 200_000), 1, SHORT);
    rig.post(rig.at(5, 3 * MS + 200_000), 2, LONG);
    rig.run_to(rig.at(12, 0));
    let (launched, _) = rig.launched();
    assert_eq!(launched.len(), 2);
    assert_eq!(launched[0].issued, rig.grid(89));
    assert_eq!(launched[1].issued, rig.grid(91));
    assert_eq!(frame_of(launched[1].done, rig.frame0), 5);
    assert_eq!(rig.shows(7), LONG);
    for frame in 0..12 {
        assert_ne!(
            rig.shows(frame),
            SHORT,
            "frame {frame} shows the earlier post"
        );
    }
}

/// A bus error on the group's second exposure byte. The group closes and nothing launches, so the
/// sensor shows the old exposure, the job is dropped, and health is degraded.
#[test]
fn an_i2c_failure_inside_the_group_closes_it_and_launches_nothing() {
    let mut rig = Rig::new();
    // The group's transactions: hold start, exposure byte 0, exposure byte 1 (fails).
    rig.bus.fail_at.store(3, Ordering::SeqCst);
    rig.post(rig.at(5, 5 * MS + MS / 2), 1, SHORT);
    rig.run_to(rig.at(6, 0));
    let group = rig.writes_from(rig.grid(93));
    let regs: Vec<(u16, bool)> = group
        .iter()
        .filter(|(r, _, _)| *r == GROUP_ACCESS || EXPOSURE_REGS.contains(r))
        .map(|(r, _, ok)| (*r, *ok))
        .collect();
    assert_eq!(
        regs,
        [
            (GROUP_ACCESS, true),
            (EXPOSURE, true),
            (EXPOSURE + 1, false),
            (GROUP_ACCESS, true),
        ]
    );
    assert_eq!(group.last().map(|(_, v, _)| *v), Some(Some(GROUP_HOLD_END)));
    assert!(
        !group
            .iter()
            .any(|(r, v, _)| *r == GROUP_ACCESS && *v == Some(GROUP_LAUNCH))
    );
    assert_eq!(rig.launched().0.len(), 0);
    assert_eq!(rig.applied_identity(), None);
    assert_eq!(
        Driver::health(&rig.sensor),
        Health::Degraded("bus error while streaming: restarting the sensor")
    );
}

/// A job read while the sensor is recovering is dropped, and never applied after the restart.
#[test]
fn a_job_posted_during_recovery_is_dropped_not_applied_after_the_restart() {
    let mut rig = Rig::new();
    rig.bus.fail_at.store(3, Ordering::SeqCst);
    rig.post(rig.at(5, 5 * MS + MS / 2), 1, SHORT);
    rig.post(rig.at(5, 10 * MS + MS / 2), 2, LONG);
    rig.run_to(rig.at(40, 0));
    let mode_writes = rig
        .writes_from(0)
        .iter()
        .filter(|(r, v, ok)| *r == MODE_SELECT && *v == Some(STREAMING) && *ok)
        .count();
    assert_eq!(mode_writes, 2, "the sensor restarted once");
    assert_eq!(rig.launched().0.len(), 0);
    assert_eq!(rig.applied_identity(), None);
    assert_eq!(Driver::health(&rig.sensor), Health::Ok);
}

/// A job posted while stopped is dropped by the next start, and never applied.
#[test]
fn a_job_posted_before_a_start_is_dropped_by_it() {
    let mut rig = Rig::new();
    rig.run_to(rig.at(5, 0));
    rig.sensor.stop().unwrap();
    rig.post(rig.at(5, 2 * MS), 1, SHORT);
    rig.sensor.start().unwrap();
    rig.next = Lifecycle::tick(&mut rig.sensor, rig.clock.load(Ordering::SeqCst));
    rig.run_to(rig.at(9, 0));
    assert_eq!(rig.launched().0.len(), 0);
    assert_eq!(rig.applied_identity(), None);
}

/// Of two posts in one poll, the later is refused by its limits: nothing is applied, and the
/// earlier one is not applied either.
#[test]
fn a_later_post_refused_by_its_limits_drops_the_earlier_one_too() {
    let mut rig = Rig::new();
    rig.post(rig.at(5, MS + 200_000), 1, SHORT);
    // 1875 lines is past this frame's exposure limit: 1822 lines less the 25-line margin.
    rig.post(rig.at(5, MS + 500_000), 2, 1875 << EXPOSURE_FRACTION_BITS);
    rig.run_to(rig.at(9, 0));
    assert_eq!(rig.launched().0.len(), 0);
    assert_eq!(rig.applied_identity(), None);
    for frame in 0..9 {
        assert_eq!(rig.shows(frame), BASE_EXPOSURE);
    }
}

impl Rig {
    fn applied_identity(&mut self) -> Option<u64> {
        self.sensor
            .applied()
            .unwrap()
            .map(|values: FrameValues| values.identity)
    }
}
