//! Register-based I2C chips on devices the board gives to the host: merged drivers read them,
//! nothing they or anything else on the device writes configures them, and every register of
//! every simulated chip is byte-for-byte what its owner left after bind, ticks and reads.
//!
//! Each chip's configuration registers hold values its owner (the operating system's driver)
//! chose, different from what the Lemnos driver would write. Nothing here is real hardware.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lemnos_contract_power_monitor::PowerReading;
use lemnos_core::{
    AddressWidth, Config, Decode, I2c, Lifecycle, MethodId, Reader, Result, Status, Vector3,
    contract, driver,
};
use lemnos_driver_bmm150::Bmm150;
use lemnos_driver_generic_sensor::GenericMagnetometer;
use lemnos_driver_ina2xx::Ina2xx;
use lemnos_driver_ina2xx::chip::{Calibration, Ina, Model};
use lemnos_hal::i2c::{I2c as _, Operation};
use lemnos_hal::register::{I2cRegisters, RegWrite, RegisterBus, RegisterError};
use lemnos_hal::{ErrorKind, HalError};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, DriverEntry, Host, Options, entry};
use lemnos_sim::{Layout, SimBoard, SimClock, SimI2c};

const HEADER: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n";

const MAG: u8 = 0x10;
const POWER: u8 = 0x40;
const DESCRIBED: u8 = 0x1e;

const READ: MethodId = MethodId {
    contract: 0,
    method: 1,
};

/// The made-up chip the description drivers' own tests describe.
const SIM_MAG: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../drivers/generic-sensor/tests/chips/sim-mag.chip"
);

/// A BMM150 its owner has powered, set to its own repetitions and rate, with a conversion in
/// its data registers.
fn bmm150(bus: &SimI2c, data_ready: bool) {
    bus.attach(MAG, Layout::Bytes);
    bus.set_bytes(MAG, 0x4b, &[0x01]); // power control: on
    bus.set_bytes(MAG, 0x40, &[0x32]); // chip ID
    bus.set_bytes(MAG, 0x4c, &[0x30]); // op mode: normal, the owner's rate
    bus.set_bytes(MAG, 0x51, &[0x17]); // the owner's XY repetitions
    bus.set_bytes(MAG, 0x52, &[0x29]); // the owner's Z repetitions
    bus.set_bytes(MAG, 0x5d, &[0, 0]);
    bus.set_bytes(MAG, 0x62, &[0, 0, 0, 0]);
    let mut rest = [0u8; 10];
    rest[0..2].copy_from_slice(&700i16.to_le_bytes());
    rest[2..4].copy_from_slice(&24000u16.to_le_bytes());
    rest[4..6].copy_from_slice(&6600u16.to_le_bytes());
    bus.set_bytes(MAG, 0x68, &rest);
    let mut data = [0u8; 8];
    data[0..2].copy_from_slice(&(200i16 << 3).to_le_bytes());
    data[2..4].copy_from_slice(&(-150i16 << 3).to_le_bytes());
    data[4..6].copy_from_slice(&(400i16 << 1).to_le_bytes());
    data[6..8].copy_from_slice(&((6600u16 << 2) | u16::from(data_ready)).to_le_bytes());
    bus.set_bytes(MAG, 0x42, &data);
}

fn word(bus: &SimI2c, address: u8, register: u8, value: u16) {
    bus.set_word(address, register, &value.to_be_bytes());
}

/// An INA226 its owner configured and calibrated for itself, reading 10 mV across the shunt
/// and 12 V on the bus.
fn ina226(bus: &SimI2c) {
    bus.attach(POWER, Layout::Words);
    word(bus, POWER, 0xfe, 0x5449);
    word(bus, POWER, 0xff, 0x2260);
    word(bus, POWER, 0x00, 0x4127); // the owner's averaging
    word(bus, POWER, 0x05, 0x0a00); // the owner's calibration
    word(bus, POWER, 0x01, 4000); // 4000 × 2.5 µV = 10 mV
    word(bus, POWER, 0x02, 9600); // 9600 × 1.25 mV = 12 V
    word(bus, POWER, 0x03, 0x1111); // scaled by the owner's calibration
    word(bus, POWER, 0x04, 0x2222);
}

/// The described chip, started by its owner in its own mode.
fn described(bus: &SimI2c) {
    bus.attach(DESCRIBED, Layout::Bytes);
    bus.set_bytes(DESCRIBED, 0x00, &[0x48]);
    bus.set_bytes(DESCRIBED, 0x09, &[0x11]); // the owner's mode, not the description's 0x1d
    bus.set_bytes(DESCRIBED, 0x0a, &[0x00]);
    bus.set_bytes(DESCRIBED, 0x01, &[0x10, 0x00]); // x: 16 counts, little-endian
    bus.set_bytes(DESCRIBED, 0x03, &[0x00, 0x20]); // y: 32 counts, big-endian
    bus.set_bytes(DESCRIBED, 0x05, &[0x30, 0x00, 0x00]); // z: 48 counts
}

/// Every register of the chip at `address`, as bytes and as words: all the sim holds.
fn registers(bus: &SimI2c, address: u8) -> Vec<(Option<u8>, Option<Vec<u8>>)> {
    (0..=255)
        .map(|register| (bus.byte(address, register), bus.word(address, register)))
        .collect()
}

fn board(owner: &str, data_ready: bool) -> (Host, SimClock, SimI2c) {
    let sim = SimBoard::new();
    let bus = sim.add_i2c("i2c");
    bmm150(&bus, data_ready);
    ina226(&bus);
    described(&bus);
    let devices = format!(
        r#"
[[devices]]
id = "mag"
driver = "bmm150"
bus = "i2c"
{owner}

[[devices]]
id = "power"
driver = "ina2xx"
bus = "i2c"
{owner}
config = {{ chip = "ina226", shunt_ohms = 0.01, max_current_a = 8.192 }}

[[devices]]
id = "described"
driver = "generic-magnetometer"
bus = "i2c"
{owner}
config = {{ address = {DESCRIBED}, description = "{SIM_MAG}" }}
"#
    );
    let (host, clock) = host_on(
        sim,
        &devices,
        &[
            entry::<Bmm150>(),
            entry::<Ina2xx>(),
            entry::<GenericMagnetometer>(),
        ],
    );
    (host, clock, bus)
}

fn host_on(sim: SimBoard, devices: &str, drivers: &[DriverEntry]) -> (Host, SimClock) {
    let clock = sim.clock();
    let host = Host::start(
        &parse(&format!("{HEADER}{devices}")).unwrap(),
        drivers,
        Box::new(sim),
        Arc::new(clock.clone()),
        Options::default(),
    )
    .unwrap();
    (host, clock)
}

/// Lets `seconds` pass a second at a time, with the host's ticks.
fn run(host: &Host, clock: &SimClock, seconds: u32) {
    host.tick();
    for _ in 0..seconds {
        clock.advance(Duration::from_secs(1));
        host.tick();
    }
}

fn read<T: for<'a> Decode<'a>>(host: &Host, id: &str) -> Result<T> {
    let device = host.device(id).unwrap().index;
    let mut reply = [0u8; 64];
    let n = host.call(device, READ, &[], &mut reply)?;
    Ok(T::decode(&mut Reader::new(&reply[..n])).unwrap())
}

/// Reads until a driver that starts its chip over several reads answers.
fn read_settled<T: for<'a> Decode<'a>>(host: &Host, clock: &SimClock, id: &str) -> T {
    for _ in 0..10 {
        match read(host, id) {
            Ok(value) => return value,
            Err(Status::AGAIN) => run(host, clock, 1),
            Err(status) => panic!("{id}: {status:?}"),
        }
    }
    panic!("{id} never answered")
}

fn close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() <= expected.abs() * 1e-4 + 1e-9,
        "{actual} != {expected}"
    );
}

#[test]
fn host_owned_register_chips_are_read_and_left_byte_for_byte_as_their_owner_set_them() {
    let (host, clock, bus) = board("owner = \"host\"", true);
    let before: Vec<_> = [MAG, POWER, DESCRIBED]
        .iter()
        .map(|address| registers(&bus, *address))
        .collect();
    run(&host, &clock, 3);
    for id in ["mag", "power", "described"] {
        assert_eq!(host.device(id).unwrap().state, DeviceState::Running, "{id}");
    }

    // Each reads at once: there is nothing to start.
    let field: Vector3 = read(&host, "mag").unwrap();
    let power: PowerReading = read(&host, "power").unwrap();
    let described: Vector3 = read(&host, "described").unwrap();
    for _ in 0..5 {
        run(&host, &clock, 1);
        assert_eq!(read::<Vector3>(&host, "mag").unwrap(), field);
        assert_eq!(read::<PowerReading>(&host, "power").unwrap(), power);
        assert_eq!(read::<Vector3>(&host, "described").unwrap(), described);
    }

    // The same values a Lemnos-owned BMM150 gives once it has started itself.
    let (ours, ours_clock, _) = board("", true);
    let expected: Vector3 = read_settled(&ours, &ours_clock, "mag");
    assert_eq!(field, expected);
    assert!(field.x.is_finite() && field.y.is_finite() && field.z.is_finite());
    // 10 mV across 10 mΩ is 1 A, at 12 V: from the shunt, not the owner's calibration.
    close(power.shunt_v, 0.010);
    close(power.bus_v, 12.0);
    close(power.current_a, 1.0);
    close(power.power_w, 12.0);
    close(described.x, 16e-6);
    close(described.y, 32e-6);
    close(described.z, 48e-6);

    host.shutdown();
    let after: Vec<_> = [MAG, POWER, DESCRIBED]
        .iter()
        .map(|address| registers(&bus, *address))
        .collect();
    assert!(before == after, "every register is as its owner left it");
}

#[test]
fn the_same_drivers_on_lemnos_owned_devices_configure_their_chips() {
    // What the host-owned run left alone is what these drivers write when the chips are theirs.
    let (host, clock, bus) = board("", true);
    let _: Vector3 = read_settled(&host, &clock, "mag");
    let _: PowerReading = read_settled(&host, &clock, "power");
    let _: Vector3 = read_settled(&host, &clock, "described");
    assert_ne!(bus.byte(MAG, 0x51), Some(0x17), "XY repetitions written");
    assert_ne!(bus.byte(MAG, 0x4c), Some(0x30), "op mode written");
    assert_ne!(bus.word(POWER, 0x05), Some(vec![0x0a, 0x00]), "calibrated");
    assert_eq!(bus.byte(DESCRIBED, 0x09), Some(0x1d), "started");
}

#[test]
fn a_host_owned_bmm150_reads_data_its_owner_has_already_taken() {
    // The owner's reads clear the data-ready bit; the registers still hold its last conversion.
    let (host, clock, _bus) = board("owner = \"host\"", false);
    run(&host, &clock, 1);
    let field: Vector3 = read(&host, "mag").unwrap();
    let (ours, ours_clock, _) = board("", true);
    assert_eq!(field, read_settled::<Vector3>(&ours, &ours_clock, "mag"));
}

#[test]
fn a_host_owned_bmm150_its_owner_powered_down_waits_and_is_not_powered_up() {
    let (host, clock, bus) = board("owner = \"host\"", true);
    bus.set_bytes(MAG, 0x4b, &[0x00]);
    let before = registers(&bus, MAG);
    for _ in 0..3 {
        assert_eq!(read::<Vector3>(&host, "mag"), Err(Status::AGAIN));
        run(&host, &clock, 1);
    }
    assert_eq!(registers(&bus, MAG), before);
    // Its owner powers it up: the next read reads.
    bus.set_bytes(MAG, 0x4b, &[0x01]);
    assert!(read::<Vector3>(&host, "mag").is_ok());
}

// Every configuring write these drivers make, made on a host-owned device's bus.

#[contract("example.configure", version = 1)]
pub trait Configure {
    /// How many configuring writes reached the bus, and how many were refused.
    #[method(1)]
    fn outcome(&mut self) -> Result<[u32; 2]>;
}

/// What happened to each attempt, shared with the test.
type Log = Arc<Mutex<Vec<(&'static str, core::result::Result<(), ErrorKind>)>>>;

static LOG: std::sync::OnceLock<Log> = std::sync::OnceLock::new();

/// Tries, from `start` and from every `tick`, each write the merged I2C sensor drivers make to
/// configure their chips, through the same bus a host-owned device's driver is given and
/// declaring the same one-byte register address they do.
#[driver("configurer", provides = [Configure])]
pub struct Configurer {
    #[needs]
    bus: I2c,
}

impl Configurer {
    fn attempt(&mut self) {
        let log = LOG.get().unwrap();
        let record = |what, outcome: core::result::Result<(), ErrorKind>| {
            log.lock().unwrap().push((what, outcome));
        };
        // bmm150: power on, repetitions, op mode.
        for (what, register, value) in [
            ("bmm150 power on", 0x4b, 0x01),
            ("bmm150 xy repetitions", 0x51, 0x04),
            ("bmm150 z repetitions", 0x52, 0x0e),
            ("bmm150 op mode", 0x4c, 0x00),
        ] {
            let outcome = I2cRegisters::new(&mut self.bus, MAG, AddressWidth::Bits8)
                .write8(register, value)
                .map_err(bus_error);
            record(what, outcome);
        }
        // ina2xx: its own start-up, which verifies the chip and then configures and calibrates.
        let ina = Ina::new(
            Model::Ina226,
            POWER,
            Calibration::from_micro(10_000, 8_192_000),
        );
        let outcome = ina
            .unwrap()
            .init(&mut self.bus)
            .map_err(|error| match error {
                lemnos_driver_ina2xx::chip::Error::Register(error) => bus_error(error),
                other => other.kind(),
            });
        record("ina226 configure and calibrate", outcome);
        let outcome = I2cRegisters::new(&mut self.bus, POWER, AddressWidth::Bits8)
            .write_sequence(&[RegWrite::word(0x00, 0x4527), RegWrite::word(0x05, 0x0800)])
            .map_err(bus_error);
        record("ina226 register words", outcome);
        // generic-magnetometer: the description's start-up writes.
        for (what, register, value) in [
            ("described soft reset", 0x0a, 0x80),
            ("described mode", 0x09, 0x1d),
        ] {
            let outcome = I2cRegisters::new(&mut self.bus, DESCRIBED, AddressWidth::Bits8)
                .write(register, 1, value)
                .map_err(bus_error);
            record(what, outcome);
        }
        // A write shaped to look like a register read: the value rides in the address write, or
        // in a second write before the read.
        let mut byte = [0u8; 1];
        record(
            "value before a read",
            self.bus.write_read(MAG, &[0x4c, 0x00], &mut byte),
        );
        record(
            "value as a second write",
            self.bus.transaction(
                MAG,
                &mut [
                    Operation::Write(&[0x4c]),
                    Operation::Write(&[0x00]),
                    Operation::Read(&mut byte),
                ],
            ),
        );
        record(
            "address with nothing read",
            self.bus.transaction(MAG, &mut [Operation::Write(&[0x4c])]),
        );
    }
}

/// The bus's own answer: a register layer's error kind goes through `embedded-hal`'s, which has
/// no word for a refusal.
fn bus_error(error: RegisterError<ErrorKind>) -> ErrorKind {
    error.bus_error().copied().unwrap_or(error.kind())
}

impl Lifecycle for Configurer {
    type Needs = ConfigurerNeeds;
    const TICKS: bool = true;
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;
    const I2C_REGISTER_ADDRESS: Option<AddressWidth> = Some(AddressWidth::Bits8);

    fn start(needs: ConfigurerNeeds, _: &Config<'_>) -> Result<Self> {
        let mut configurer = Self { bus: needs.bus };
        configurer.attempt();
        Ok(configurer)
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        self.attempt();
        Some(now_ns + 1_000_000_000)
    }
}

impl Configure for Configurer {
    fn outcome(&mut self) -> Result<[u32; 2]> {
        let log = LOG.get().unwrap().lock().unwrap();
        let refused = log.iter().filter(|(_, outcome)| outcome.is_err()).count() as u32;
        Ok([log.len() as u32 - refused, refused])
    }
}

#[test]
fn every_configuring_write_on_a_host_owned_bus_is_refused_before_it_reaches_a_chip() {
    LOG.set(Arc::default()).unwrap();
    let sim = SimBoard::new();
    let bus = sim.add_i2c("i2c");
    bmm150(&bus, true);
    ina226(&bus);
    described(&bus);
    let before: Vec<_> = [MAG, POWER, DESCRIBED]
        .iter()
        .map(|address| registers(&bus, *address))
        .collect();
    let devices =
        "[[devices]]\nid = \"c\"\ndriver = \"configurer\"\nbus = \"i2c\"\nowner = \"host\"\n";
    let (host, clock) = host_on(sim, devices, &[entry::<Configurer>()]);
    assert_eq!(host.device("c").unwrap().state, DeviceState::Running);
    run(&host, &clock, 3);
    host.shutdown();

    let log = LOG.get().unwrap().lock().unwrap();
    assert!(
        log.len() >= 4 * 12,
        "attempted from start and from each tick: {}",
        log.len()
    );
    for (what, outcome) in log.iter() {
        assert_eq!(
            *outcome,
            Err(ErrorKind::PermissionDenied),
            "{what} was not refused"
        );
    }
    let after: Vec<_> = [MAG, POWER, DESCRIBED]
        .iter()
        .map(|address| registers(&bus, *address))
        .collect();
    assert!(before == after, "no register changed");
}
