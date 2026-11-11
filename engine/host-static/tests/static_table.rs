//! Two real drivers in a static table: one that talks to a bus and one built from it, bound and
//! called the way firmware would, with nothing allocated from start to steady state.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use embedded_hal::i2c::{self, Operation};
use lemnos_core::{
    Config, ConfigValue, Decode, Description, Health, MethodId, Reader, Status, Vector3,
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
}

impl lemnos_core::HostClock for Steps {
    fn now_ns(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

const FIRST: u8 = 0x18;
const SECOND: u8 = 0x68;
/// One g at the first target's default range (6 g full scale over 32768 counts).
const ONE_G: i16 = 5461;

/// Two register files on one bus, as the embedded-hal peripheral a firmware has. Counts its
/// transactions.
struct TwoTargets {
    files: [[u8; 256]; 2],
    pointer: u8,
    transactions: &'static AtomicU32,
}

#[derive(Debug)]
struct NoAck;

impl i2c::Error for NoAck {
    fn kind(&self) -> i2c::ErrorKind {
        i2c::ErrorKind::NoAcknowledge(i2c::NoAcknowledgeSource::Address)
    }
}

impl i2c::ErrorType for TwoTargets {
    type Error = NoAck;
}

impl i2c::I2c for TwoTargets {
    fn transaction(&mut self, address: u8, ops: &mut [Operation<'_>]) -> Result<(), NoAck> {
        self.transactions.fetch_add(1, Ordering::Relaxed);
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

/// The chip IDs in place, the first target reading +1 g on Z and the second at rest.
fn two_targets(transactions: &'static AtomicU32) -> TwoTargets {
    let mut files = [[0; 256]; 2];
    files[0][0x00] = 0x1e;
    files[0][0x16..0x18].copy_from_slice(&ONE_G.to_le_bytes());
    files[1][0x00] = 0x0f;
    TwoTargets {
        files,
        pointer: 0,
        transactions,
    }
}

type I2cStall = Stall<DynI2c, Locked<Option<&'static mut DynI2c>>>;

/// The bus the firmware names `i2c0`, with the two targets on it, in a stall of its own: the
/// firmware's `static`, filled once at boot. The bus is made before anything is counted.
macro_rules! i2c0 {
    ($transactions:expr) => {{
        static STALL: I2cStall = Stall::new("i2c0", Locked::new(None));
        let bus: &'static mut DynI2c = Box::leak(Box::new(I2cBus(two_targets($transactions))));
        assert!(STALL.put(bus));
        [&STALL as &'static dyn Stalled<DynI2c>]
    }};
}

const READ: u16 = 1;

fn method(contract: u8) -> MethodId {
    MethodId {
        contract,
        method: READ,
    }
}

fn read(table: &Table, device: u32, contract: u8) -> Result<Vector3, Status> {
    let mut reply = [0; 16];
    let n = table.call(device, method(contract), &[], &mut reply)?;
    Vector3::decode(&mut Reader::new(&reply[..n]))
}

/// Reads until the dies have powered up (each read takes one step) and returns the first value.
fn first_value(table: &Table, device: u32, contract: u8) -> Vector3 {
    for _ in 0..20 {
        match read(table, device, contract) {
            Ok(value) => return value,
            Err(status) => assert_eq!(status, Status::AGAIN),
        }
    }
    panic!("device {device} never answered");
}

type LeafSlot = Slot<Bmi088, Locked<Device<Bmi088>>>;
type FusedSlot = Slot<Orientation, Locked<Device<Orientation>>>;

const SENSOR_SPEC: Spec = Spec::new("sensor").bus("i2c0");
const FUSED_SPEC: Spec = Spec::new("fused")
    .uses(&[("accel", "sensor"), ("gyro", "sensor")])
    .config(Config::new(&[("sample_rate_hz", ConfigValue::Int(100))]));

/// Device numbers: the parent comes first, so binding has to put the child first itself.
const FUSED: u32 = 0;
const SENSOR: u32 = 1;

macro_rules! table {
    ($table:ident, $clock:ident) => {
        static $clock: Steps = Steps::new();
        static SENSOR_SLOT: LeafSlot = Slot::new(SENSOR_SPEC, Locked::new(Device::new()));
        static FUSED_SLOT: FusedSlot = Slot::new(FUSED_SPEC, Locked::new(Device::new()));
        static $table: Table = Table::new(&[&FUSED_SLOT, &SENSOR_SLOT], &$clock, Options::DEFAULT);
    };
}

#[test]
fn both_drivers_bind_children_first_and_answer_through_the_table() {
    table!(TABLE, CLOCK);
    static TRANSACTIONS: AtomicU32 = AtomicU32::new(0);
    let i2c = i2c0!(&TRANSACTIONS);
    TABLE.start(&mut Buses::new().with_i2c(&i2c));
    assert_eq!(TABLE.state(SENSOR), Some(State::Running));
    assert_eq!(TABLE.state(FUSED), Some(State::Running));

    let angles = first_value(&TABLE, FUSED, 0);
    assert!(angles.x.abs() < 1e-3 && angles.y.abs() < 1e-3, "{angles:?}");
    let before = TRANSACTIONS.load(Ordering::Relaxed);
    read(&TABLE, FUSED, 0).unwrap();
    // One read of each die, each a register write then a burst read in one transaction.
    assert_eq!(TRANSACTIONS.load(Ordering::Relaxed) - before, 2);
}

#[test]
fn a_call_reaches_the_device_and_contract_it_names() {
    table!(TABLE, CLOCK);
    static TRANSACTIONS: AtomicU32 = AtomicU32::new(0);
    let i2c = i2c0!(&TRANSACTIONS);
    TABLE.start(&mut Buses::new().with_i2c(&i2c));

    let acceleration = first_value(&TABLE, SENSOR, 0);
    assert!((acceleration.z - 9.806).abs() < 0.01, "{acceleration:?}");
    assert!(acceleration.x == 0.0 && acceleration.y == 0.0);
    let rate = first_value(&TABLE, SENSOR, 1);
    assert_eq!((rate.x, rate.y, rate.z), (0.0, 0.0, 0.0));
    assert_eq!(read(&TABLE, 7, 0), Err(Status::NOT_FOUND));
    let mut reply = [0; 16];
    let unknown = MethodId {
        contract: 0,
        method: 99,
    };
    assert!(TABLE.call(SENSOR, unknown, &[], &mut reply).is_err());
}

#[test]
fn nothing_allocates_from_start_to_steady_state() {
    table!(TABLE, CLOCK);
    static TRANSACTIONS: AtomicU32 = AtomicU32::new(0);
    // The firmware's buses exist before the host starts.
    let i2c = i2c0!(&TRANSACTIONS);
    let mut buses = Buses::new().with_i2c(&i2c);
    let mut reads = 0;
    let ((), count) = allocations(|| {
        TABLE.start(&mut buses);
        for _ in 0..1000 {
            CLOCK.advance_ms(10);
            TABLE.poll(&mut buses);
            if read(&TABLE, FUSED, 0).is_ok() {
                reads += 1;
            }
            let mut description = Description::new();
            TABLE.describe(SENSOR, &mut description).unwrap();
            let _ = TABLE.health(FUSED);
        }
    });
    assert!(reads > 990, "{reads} reads answered");
    assert_eq!(count, 0, "allocations from start to steady state");
}

#[test]
fn a_device_whose_bus_is_missing_retries_forever_capped_at_eight_seconds() {
    table!(TABLE, CLOCK);
    TABLE.start(&mut Buses::new());
    let mut waits = Vec::new();
    for _ in 0..7 {
        let Some(State::Backoff {
            next_retry_ns,
            reason,
            ..
        }) = TABLE.state(SENSOR)
        else {
            panic!("{:?}", TABLE.state(SENSOR));
        };
        assert_eq!(reason, Status::NO_DEVICE);
        waits.push((next_retry_ns - lemnos_core::HostClock::now_ns(&CLOCK)) / 1_000_000_000);
        CLOCK.advance_ms(
            next_retry_ns / 1_000_000 - lemnos_core::HostClock::now_ns(&CLOCK) / 1_000_000,
        );
        TABLE.poll(&mut Buses::new());
    }
    assert_eq!(waits, [1, 2, 4, 8, 8, 8, 8]);
}

#[test]
fn a_parent_waits_for_its_child_without_using_up_a_bus_then_binds() {
    table!(TABLE, CLOCK);
    static TRANSACTIONS: AtomicU32 = AtomicU32::new(0);
    TABLE.start(&mut Buses::new());
    assert!(matches!(TABLE.state(FUSED), Some(State::Backoff { .. })));
    assert_eq!(TABLE.health(FUSED), Health::Failed("retrying"));

    let i2c = i2c0!(&TRANSACTIONS);
    let mut buses = Buses::new().with_i2c(&i2c);
    let mut next = Some(0);
    for _ in 0..10 {
        CLOCK.0.store(next.unwrap(), Ordering::Relaxed);
        next = TABLE.poll(&mut buses);
        if TABLE.state(FUSED) == Some(State::Running) {
            break;
        }
    }
    assert_eq!(TABLE.state(SENSOR), Some(State::Running));
    assert_eq!(TABLE.state(FUSED), Some(State::Running));
    first_value(&TABLE, FUSED, 0);
}

#[test]
fn health_passes_up_and_off_and_on_are_immediate() {
    table!(TABLE, CLOCK);
    static TRANSACTIONS: AtomicU32 = AtomicU32::new(0);
    let i2c = i2c0!(&TRANSACTIONS);
    TABLE.start(&mut Buses::new().with_i2c(&i2c));
    first_value(&TABLE, FUSED, 0);
    assert_eq!(
        TABLE.health(FUSED),
        Health::Degraded("six-axis: yaw is relative")
    );

    TABLE.off(SENSOR).unwrap();
    assert_eq!(TABLE.state(SENSOR), Some(State::Off));
    assert_eq!(
        TABLE.health(FUSED),
        Health::Failed("a required device failed")
    );
    assert_eq!(read(&TABLE, FUSED, 0), Err(Status::NO_DEVICE));

    TABLE.on(SENSOR).unwrap();
    assert_eq!(TABLE.state(SENSOR), Some(State::Running));
    read(&TABLE, FUSED, 0).unwrap();
}

#[test]
fn devices_are_found_by_id_and_tag_and_describe_themselves() {
    table!(TABLE, CLOCK);
    static TRANSACTIONS: AtomicU32 = AtomicU32::new(0);
    let i2c = i2c0!(&TRANSACTIONS);
    TABLE.start(&mut Buses::new().with_i2c(&i2c));
    assert_eq!(TABLE.index("sensor"), Some(SENSOR));
    assert_eq!(TABLE.id(FUSED), Some("fused"));
    assert_eq!(TABLE.find("orientation/1", 0), Some(FUSED));
    assert_eq!(TABLE.find("gyroscope/1", 0), Some(SENSOR));
    assert_eq!(TABLE.find("accelerometer/1", SENSOR + 1), None);
    let mut description = Description::new();
    TABLE.describe(FUSED, &mut description).unwrap();
    assert!(description.find("roll").is_some());
    TABLE.describe(SENSOR, &mut description).unwrap();
    assert!(description.find("acceleration.z").is_some());
}
