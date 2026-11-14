//! A static table drops a driver and binds it again, as the main host does: when the driver
//! says what it was bound with is gone, and when its calls keep failing the way a gone device's
//! do. The bus the dropped driver held comes back to its stall, so the next bind takes it again,
//! and nothing is allocated on the way.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use embedded_hal::i2c::{self, I2c as _, Operation};
use lemnos_core::{
    Config, ConfigError, ConfigValue, Decode, Health, HostClock, I2c, Key, Kind, Lifecycle,
    MethodId, Reader, Result, Status, Vector3, contract, driver,
};
use lemnos_driver_bmi088::Bmi088;
use lemnos_driver_orientation::Orientation;
use lemnos_host_static::{Device, Lock, Options, Slot, Spec, State, Table};
use lemnos_mcu::{Buses, DynI2c, I2cBus, Stall, Stalled};
use lemnos_measure::test_alloc::{Counting, allocations};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The firmware's lock, played by a mutex.
struct Locked<T>(Mutex<T>);

impl<T> Locked<T> {
    const fn new(value: T) -> Self {
        Self(Mutex::new(value))
    }
}

impl<T: Send> Lock for Locked<T> {
    type Inner = T;

    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        self.0.try_lock().ok().map(|mut value| f(&mut value))
    }
}

/// A clock the test moves.
struct Steps(AtomicU64);

impl Steps {
    const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    fn advance_ms(&self, ms: u64) {
        self.0.fetch_add(ms * 1_000_000, Ordering::Relaxed);
    }

    fn set(&self, ns: u64) {
        self.0.store(ns, Ordering::Relaxed);
    }
}

impl HostClock for Steps {
    fn now_ns(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

const SECOND_NS: u64 = 1_000_000_000;

/// What a test tells its driver and its bus, and what they tell it back. One per test, picked
/// by the device's `case` setting.
struct Case {
    /// The driver's health says what it was bound with is gone.
    gone: AtomicBool,
    /// The bus answers nothing.
    down: AtomicBool,
    /// The driver's `start` refuses, after it was handed the bus.
    refuse: AtomicBool,
    binds: AtomicU32,
    drops: AtomicU32,
    ticks: AtomicU32,
}

impl Case {
    const fn new() -> Self {
        Self {
            gone: AtomicBool::new(false),
            down: AtomicBool::new(false),
            refuse: AtomicBool::new(false),
            binds: AtomicU32::new(0),
            drops: AtomicU32::new(0),
            ticks: AtomicU32::new(0),
        }
    }
}

static CASES: [Case; 8] = [const { Case::new() }; 8];

/// A bus with one byte to read, which stops answering when its case says so.
struct Wire(&'static Case);

#[derive(Debug)]
struct NoAck;

impl i2c::Error for NoAck {
    fn kind(&self) -> i2c::ErrorKind {
        i2c::ErrorKind::NoAcknowledge(i2c::NoAcknowledgeSource::Address)
    }
}

impl i2c::ErrorType for Wire {
    type Error = NoAck;
}

impl i2c::I2c for Wire {
    fn transaction(&mut self, _: u8, ops: &mut [Operation<'_>]) -> core::result::Result<(), NoAck> {
        if self.0.down.load(Ordering::Relaxed) {
            return Err(NoAck);
        }
        for op in ops {
            if let Operation::Read(out) = op {
                out.fill(0x5A);
            }
        }
        Ok(())
    }
}

#[contract("example.probe", version = 1)]
pub trait Probing {
    /// One byte off the bus. `IO` when the bus doesn't answer.
    #[method(1)]
    fn read(&mut self) -> Result<u8>;
    /// Refused, in a way that says nothing about the device being there.
    #[method(2)]
    fn wrong(&mut self) -> Result<u8>;
}

/// A driver that holds a bus and can say it is gone.
#[driver("probe", provides = [Probing])]
pub struct Probe {
    #[needs]
    bus: I2c,
    case: &'static Case,
}

impl Lifecycle for Probe {
    type Needs = ProbeNeeds;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(&[Key::optional("case", Kind::Int)])
    }
    const TICKS: bool = true;

    fn start(needs: ProbeNeeds, cfg: &Config<'_>) -> Result<Self> {
        let case = &CASES[cfg.int("case").ok_or(Status::INVALID_ARGUMENT)? as usize];
        if case.refuse.load(Ordering::Relaxed) {
            // `needs` is dropped here, with the bus in it.
            return Err(Status::IO);
        }
        case.binds.fetch_add(1, Ordering::Relaxed);
        Ok(Self {
            bus: needs.bus,
            case,
        })
    }

    fn health(&self) -> Health {
        if self.case.gone.load(Ordering::Relaxed) {
            Health::Gone("the chip left")
        } else {
            Health::Ok
        }
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        self.case.ticks.fetch_add(1, Ordering::Relaxed);
        Some(now_ns + SECOND_NS / 10)
    }
}

impl Probing for Probe {
    fn read(&mut self) -> Result<u8> {
        let mut byte = [0u8; 1];
        self.bus.read(0x18, &mut byte).map_err(|_| Status::IO)?;
        Ok(byte[0])
    }

    fn wrong(&mut self) -> Result<u8> {
        Err(Status::INVALID_ARGUMENT)
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.case.drops.fetch_add(1, Ordering::Relaxed);
    }
}

type I2cStall = Stall<DynI2c, Locked<Option<&'static mut DynI2c>>>;
type ProbeSlot = Slot<Probe, Locked<Device<Probe>>>;

/// A table of one probe on a bus in a stall, for case `$case`: `$table`, `$clock`, `$stall`.
macro_rules! probe {
    ($case:literal, $table:ident, $clock:ident, $stall:ident) => {
        static $clock: Steps = Steps::new();
        static $stall: I2cStall = Stall::new("i2c0", Locked::new(None));
        static SLOT: ProbeSlot = Slot::new(
            Spec::new("probe")
                .bus("i2c0")
                .config(Config::new(&[("case", ConfigValue::Int($case))])),
            Locked::new(Device::new()),
        );
        static $table: Table = Table::new(&[&SLOT], &$clock, Options::DEFAULT);
        let bus: &'static mut DynI2c = Box::leak(Box::new(I2cBus(Wire(&CASES[$case]))));
        assert!($stall.put(bus));
    };
}

fn method(method: u16) -> MethodId {
    MethodId {
        contract: 0,
        method,
    }
}

fn read(table: &Table) -> Result<u8> {
    let mut reply = [0u8; 4];
    table.call(0, method(1), &[], &mut reply)?;
    Ok(reply[0])
}

/// How long the device waits for its next bind, in whole seconds, and why.
fn waiting(table: &Table, clock: &Steps) -> (u64, u32, Status) {
    let Some(State::Backoff {
        attempts,
        next_retry_ns,
        reason,
    }) = table.state(0)
    else {
        panic!("{:?}", table.state(0));
    };
    (
        (next_retry_ns - clock.now_ns()) / SECOND_NS,
        attempts,
        reason,
    )
}

#[test]
fn a_driver_that_says_it_is_gone_is_dropped_gives_its_bus_back_and_is_bound_again() {
    probe!(0, TABLE, CLOCK, STALL);
    let case = &CASES[0];
    let stalls = [&STALL as &'static dyn Stalled<DynI2c>];
    let mut buses = Buses::new().with_i2c(&stalls);
    TABLE.start(&mut buses);
    assert_eq!(TABLE.state(0), Some(State::Running));
    assert!(!STALL.is_in(), "the driver has the bus");
    assert_eq!(read(&TABLE), Ok(0x5A));

    case.gone.store(true, Ordering::Relaxed);
    // The call's own answer stands; the driver goes after it.
    assert_eq!(read(&TABLE), Ok(0x5A));
    assert_eq!(waiting(&TABLE, &CLOCK), (1, 1, Status::NO_DEVICE));
    assert_eq!(case.drops.load(Ordering::Relaxed), 1);
    assert!(STALL.is_in(), "the bus came back by itself");
    assert_eq!(read(&TABLE), Err(Status::NO_DEVICE));
    assert_eq!(TABLE.health(0), Health::Failed("retrying"));

    // Not before its wait is over, and then at once.
    case.gone.store(false, Ordering::Relaxed);
    CLOCK.advance_ms(999);
    TABLE.poll(&mut buses);
    assert_eq!(case.binds.load(Ordering::Relaxed), 1);
    CLOCK.advance_ms(1);
    TABLE.poll(&mut buses);
    assert_eq!(TABLE.state(0), Some(State::Running));
    assert_eq!(case.binds.load(Ordering::Relaxed), 2);
    assert!(!STALL.is_in(), "taken again by the new driver");
    assert_eq!(read(&TABLE), Ok(0x5A));
    assert_eq!(case.drops.load(Ordering::Relaxed), 1);
}

#[test]
fn a_driver_that_only_ticks_is_seen_to_be_gone_at_its_tick() {
    probe!(1, TABLE, CLOCK, STALL);
    let case = &CASES[1];
    let stalls = [&STALL as &'static dyn Stalled<DynI2c>];
    let mut buses = Buses::new().with_i2c(&stalls);
    TABLE.start(&mut buses);
    TABLE.poll(&mut buses);
    let ticked = case.ticks.load(Ordering::Relaxed);
    assert!(ticked >= 1);
    case.gone.store(true, Ordering::Relaxed);
    // Nobody calls it. Its next tick is when the table looks.
    TABLE.poll(&mut buses);
    assert_eq!(TABLE.state(0), Some(State::Running), "no tick was due");
    CLOCK.advance_ms(100);
    TABLE.poll(&mut buses);
    assert_eq!(case.ticks.load(Ordering::Relaxed), ticked + 1);
    assert!(matches!(TABLE.state(0), Some(State::Backoff { .. })));
    assert!(STALL.is_in());
}

#[test]
fn calls_that_fail_as_a_gone_devices_do_five_times_in_a_row_get_the_driver_bound_again() {
    probe!(2, TABLE, CLOCK, STALL);
    let case = &CASES[2];
    let stalls = [&STALL as &'static dyn Stalled<DynI2c>];
    let mut buses = Buses::new().with_i2c(&stalls);
    TABLE.start(&mut buses);
    case.down.store(true, Ordering::Relaxed);
    for _ in 0..4 {
        assert_eq!(read(&TABLE), Err(Status::IO));
    }
    assert_eq!(TABLE.state(0), Some(State::Running), "four is not five");
    // One good call, and the count starts again.
    case.down.store(false, Ordering::Relaxed);
    assert_eq!(read(&TABLE), Ok(0x5A));
    case.down.store(true, Ordering::Relaxed);
    for _ in 0..4 {
        assert_eq!(read(&TABLE), Err(Status::IO));
    }
    // A refusal that says nothing about the device being there doesn't count.
    let mut reply = [0u8; 4];
    for _ in 0..10 {
        assert_eq!(
            TABLE.call(0, method(2), &[], &mut reply),
            Err(Status::INVALID_ARGUMENT)
        );
    }
    assert_eq!(TABLE.state(0), Some(State::Running));
    assert_eq!(read(&TABLE), Err(Status::IO));
    assert_eq!(waiting(&TABLE, &CLOCK), (1, 1, Status::NO_DEVICE));
    assert_eq!(case.drops.load(Ordering::Relaxed), 1);
    assert!(STALL.is_in());

    case.down.store(false, Ordering::Relaxed);
    CLOCK.advance_ms(1000);
    TABLE.poll(&mut buses);
    assert_eq!(read(&TABLE), Ok(0x5A));
    assert_eq!(case.binds.load(Ordering::Relaxed), 2);
}

#[test]
fn a_driver_that_goes_straight_after_each_bind_waits_longer_each_time_up_to_eight_seconds() {
    probe!(3, TABLE, CLOCK, STALL);
    let case = &CASES[3];
    let stalls = [&STALL as &'static dyn Stalled<DynI2c>];
    let mut buses = Buses::new().with_i2c(&stalls);
    TABLE.start(&mut buses);
    case.gone.store(true, Ordering::Relaxed);
    let _ = read(&TABLE);
    let mut waits = Vec::new();
    for _ in 0..6 {
        let (wait, _, _) = waiting(&TABLE, &CLOCK);
        waits.push(wait);
        CLOCK.advance_ms(wait * 1000);
        // Bound again, ticked, and gone at that first look.
        TABLE.poll(&mut buses);
    }
    assert_eq!(waits, [1, 2, 4, 8, 8, 8]);
    assert_eq!(case.binds.load(Ordering::Relaxed), 7);

    // A run as long as the cap clears it: the next wait is the first again.
    case.gone.store(false, Ordering::Relaxed);
    CLOCK.advance_ms(waiting(&TABLE, &CLOCK).0 * 1000);
    TABLE.poll(&mut buses);
    assert_eq!(read(&TABLE), Ok(0x5A));
    CLOCK.advance_ms(8000);
    case.gone.store(true, Ordering::Relaxed);
    let _ = read(&TABLE);
    assert_eq!(waiting(&TABLE, &CLOCK).0, 1);
}

#[test]
fn a_bind_that_fails_after_taking_the_bus_gives_it_back() {
    probe!(4, TABLE, CLOCK, STALL);
    let case = &CASES[4];
    case.refuse.store(true, Ordering::Relaxed);
    let stalls = [&STALL as &'static dyn Stalled<DynI2c>];
    let mut buses = Buses::new().with_i2c(&stalls);
    TABLE.start(&mut buses);
    assert_eq!(waiting(&TABLE, &CLOCK), (1, 1, Status::IO));
    assert!(STALL.is_in(), "the refused driver's bus is back");
    case.refuse.store(false, Ordering::Relaxed);
    CLOCK.advance_ms(1000);
    TABLE.poll(&mut buses);
    assert_eq!(TABLE.state(0), Some(State::Running));
    assert_eq!(read(&TABLE), Ok(0x5A));
}

#[test]
fn dropping_and_binding_again_allocates_nothing() {
    probe!(5, TABLE, CLOCK, STALL);
    let case = &CASES[5];
    let stalls = [&STALL as &'static dyn Stalled<DynI2c>];
    let mut buses = Buses::new().with_i2c(&stalls);
    TABLE.start(&mut buses);
    let ((), count) = allocations(|| {
        for _ in 0..200 {
            assert_eq!(read(&TABLE), Ok(0x5A));
            case.gone.store(true, Ordering::Relaxed);
            let _ = read(&TABLE);
            case.gone.store(false, Ordering::Relaxed);
            // Long enough up that every wait is the first.
            CLOCK.advance_ms(9000);
            TABLE.poll(&mut buses);
            CLOCK.advance_ms(9000);
        }
    });
    assert_eq!(case.binds.load(Ordering::Relaxed), 201);
    assert_eq!(count, 0, "allocations in 200 rebinds");
}

// The real drivers: a sensor on a bus that stops answering, and the device built on it.

const FIRST: u8 = 0x18;
const SECOND: u8 = 0x68;

/// Two register files on one bus, which answers nothing while `DOWN` is set.
struct TwoTargets {
    files: [[u8; 256]; 2],
    pointer: u8,
}

static DOWN: AtomicBool = AtomicBool::new(false);

impl i2c::ErrorType for TwoTargets {
    type Error = NoAck;
}

impl i2c::I2c for TwoTargets {
    fn transaction(
        &mut self,
        address: u8,
        ops: &mut [Operation<'_>],
    ) -> core::result::Result<(), NoAck> {
        if DOWN.load(Ordering::Relaxed) {
            return Err(NoAck);
        }
        let file = match address {
            FIRST => &mut self.files[0],
            SECOND => &mut self.files[1],
            _ => return Err(NoAck),
        };
        for op in ops {
            match op {
                Operation::Write(bytes) => {
                    if let Some((first, rest)) = bytes.split_first() {
                        self.pointer = *first;
                        for byte in rest {
                            file[usize::from(self.pointer)] = *byte;
                            self.pointer = self.pointer.wrapping_add(1);
                        }
                    }
                }
                Operation::Read(out) => {
                    for byte in out.iter_mut() {
                        *byte = file[usize::from(self.pointer)];
                        self.pointer = self.pointer.wrapping_add(1);
                    }
                }
            }
        }
        Ok(())
    }
}

fn fused(table: &Table) -> core::result::Result<Vector3, Status> {
    let mut reply = [0; 16];
    let n = table.call(0, method(1), &[], &mut reply)?;
    Vector3::decode(&mut Reader::new(&reply[..n]))
}

#[test]
fn a_sensor_whose_bus_stops_answering_is_bound_again_and_the_device_on_it_recovers() {
    static CLOCK: Steps = Steps::new();
    static STALL: I2cStall = Stall::new("i2c0", Locked::new(None));
    static SENSOR: Slot<Bmi088, Locked<Device<Bmi088>>> =
        Slot::new(Spec::new("sensor").bus("i2c0"), Locked::new(Device::new()));
    static FUSED: Slot<Orientation, Locked<Device<Orientation>>> = Slot::new(
        Spec::new("fused")
            .uses(&[("accel", "sensor"), ("gyro", "sensor")])
            .config(Config::new(&[("sample_rate_hz", ConfigValue::Int(100))])),
        Locked::new(Device::new()),
    );
    static TABLE: Table = Table::new(&[&FUSED, &SENSOR], &CLOCK, Options::DEFAULT);
    let mut files = [[0; 256]; 2];
    files[0][0x00] = 0x1e;
    files[0][0x16..0x18].copy_from_slice(&5461i16.to_le_bytes());
    files[1][0x00] = 0x0f;
    let bus: &'static mut DynI2c = Box::leak(Box::new(I2cBus(TwoTargets { files, pointer: 0 })));
    assert!(STALL.put(bus));
    let stalls = [&STALL as &'static dyn Stalled<DynI2c>];
    let mut buses = Buses::new().with_i2c(&stalls);
    TABLE.start(&mut buses);
    let answers = |within: u32| {
        (0..within).any(|_| {
            CLOCK.advance_ms(10);
            TABLE.poll(&mut Buses::new().with_i2c(&stalls));
            fused(&TABLE).is_ok()
        })
    };
    assert!(answers(20), "up after power-on");

    // The wire goes: every read fails, and after enough of them the sensor's driver is let go.
    DOWN.store(true, Ordering::Relaxed);
    let mut dropped = false;
    for _ in 0..200 {
        CLOCK.advance_ms(10);
        TABLE.poll(&mut buses);
        assert!(fused(&TABLE).is_err());
        if matches!(TABLE.state(1), Some(State::Backoff { .. })) {
            dropped = true;
            break;
        }
    }
    assert!(dropped, "the sensor was never let go: {:?}", TABLE.state(1));
    assert!(STALL.is_in(), "its bus is back in the stall");
    assert_ne!(TABLE.health(0), Health::Ok);

    // The wire comes back. Nothing is restarted by hand: the table binds what it dropped, and
    // the device built on the sensor answers again, within the retry cap of each.
    DOWN.store(false, Ordering::Relaxed);
    let mut up = false;
    for _ in 0..40 {
        let next = TABLE.poll(&mut buses).unwrap_or(CLOCK.now_ns() + SECOND_NS);
        CLOCK.set(next.max(CLOCK.now_ns() + 10_000_000));
        TABLE.poll(&mut buses);
        if answers(20) {
            up = true;
            break;
        }
    }
    assert!(up, "{:?} {:?}", TABLE.state(0), TABLE.state(1));
    assert_eq!(TABLE.state(1), Some(State::Running));
    assert!(!STALL.is_in());
}
