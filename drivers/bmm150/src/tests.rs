//! The driver on a simulated bus: the chip's registers are the sim's bytes.

use super::*;
use crate::chip::{
    CHIP_ID, REG_CHIP_ID, REG_DATA, REG_OP_MODE, REG_POWER, REG_REP_XY, REG_REP_Z, REG_TRIM_X1,
    REG_TRIM_Z2, REG_TRIM_Z4, RawSample, Trim,
};
use lemnos_contract_self_test::{Outcome, Record, SelfTest, read_tests, reason};
use lemnos_core::{
    Description, Driver, ItemKind, JobState, Lifecycle, Reader, Resources, Status, Writer,
};
use lemnos_sim::{Layout, SimBoard, SimI2c};

const ADDRESS: u8 = DEFAULT_ADDRESS;
const XYZ1: u16 = 6600;
const Z1: u16 = 24000;
const Z2: i16 = 700;

/// The simulated bus takes register numbers as bytes.
fn reg(register: u16) -> u8 {
    register as u8
}

/// The trim blocks after X/Y: a neutral X/Y trim and realistic Z trim.
fn trim_rest() -> [u8; 10] {
    let mut rest = [0u8; 10];
    rest[0..2].copy_from_slice(&Z2.to_le_bytes());
    rest[2..4].copy_from_slice(&Z1.to_le_bytes());
    rest[4..6].copy_from_slice(&XYZ1.to_le_bytes());
    rest
}

fn trim() -> Trim {
    Trim::from_registers([0, 0], [0, 0, 0, 0], trim_rest())
}

/// A board with one chip at the default address, its ID and trim in place.
fn chip() -> (SimBoard, SimI2c) {
    let board = SimBoard::new();
    let sim = board.add_i2c("bus");
    sim.attach(ADDRESS, Layout::Bytes);
    sim.set_bytes(ADDRESS, reg(REG_CHIP_ID), &[CHIP_ID]);
    sim.set_bytes(ADDRESS, reg(REG_TRIM_X1), &[0, 0]);
    sim.set_bytes(ADDRESS, reg(REG_TRIM_Z4), &[0, 0, 0, 0]);
    sim.set_bytes(ADDRESS, reg(REG_TRIM_Z2), &trim_rest());
    (board, sim)
}

/// Binds the driver to `board` with the given config entries.
fn bind(board: &mut SimBoard, entries: &[(&str, ConfigValue<'_>)]) -> Result<Bmm150> {
    let config = Config::new(entries);
    let mut res = Resources::new(board, None);
    Bmm150::bind(&mut res, &config)
}

fn data(x: i16, y: i16, z: i16, rhall: u16) -> [u8; 8] {
    let mut bytes = [0u8; 8];
    bytes[0..2].copy_from_slice(&(x << 3).to_le_bytes());
    bytes[2..4].copy_from_slice(&(y << 3).to_le_bytes());
    bytes[4..6].copy_from_slice(&(z << 1).to_le_bytes());
    bytes[6..8].copy_from_slice(&((rhall << 2) | 1).to_le_bytes());
    bytes
}

/// Data bytes with the data-ready bit clear: a conversion already read.
fn stale(x: i16, y: i16, z: i16, rhall: u16) -> [u8; 8] {
    let mut bytes = data(x, y, z, rhall);
    bytes[6] &= !1;
    bytes
}

/// Checks a tesla value against microtesla, to 0.1 µT.
fn close_ut(actual_t: f32, expected_ut: f32) {
    let actual_ut = actual_t * 1.0e6;
    assert!(
        (actual_ut - expected_ut).abs() <= 0.1,
        "{actual_ut} uT != {expected_ut} uT"
    );
}

#[test]
fn power_up_takes_one_read_then_configures() {
    let (mut board, sim) = chip();
    sim.set_bytes(ADDRESS, reg(REG_DATA), &data(0, 0, 0, XYZ1));
    let mut mag = bind(
        &mut board,
        &[
            ("preset", ConfigValue::Str("enhanced")),
            ("data_rate", ConfigValue::Int(30)),
        ],
    )
    .unwrap();
    assert_eq!(Driver::health(&mag), Health::Failed("not started"));

    assert_eq!(mag.read(), Err(Status::AGAIN));
    assert_eq!(sim.byte(ADDRESS, reg(REG_POWER)), Some(0x01));
    assert_eq!(sim.byte(ADDRESS, reg(REG_REP_XY)), None);
    assert_eq!(Driver::health(&mag), Health::Degraded("powering up"));

    mag.read().unwrap();
    assert_eq!(sim.byte(ADDRESS, reg(REG_REP_XY)), Some(0x07));
    assert_eq!(sim.byte(ADDRESS, reg(REG_REP_Z)), Some(0x1a));
    // 30 Hz (0b111) in bits 5-3, normal mode.
    assert_eq!(sim.byte(ADDRESS, reg(REG_OP_MODE)), Some(0x38));
    assert_eq!(Driver::health(&mag), Health::Ok);
}

#[test]
fn config_selects_preset_and_rate() {
    let (mut board, sim) = chip();
    sim.set_bytes(ADDRESS, reg(REG_DATA), &data(0, 0, 0, XYZ1));
    let mut mag = bind(
        &mut board,
        &[
            ("preset", ConfigValue::Str("high_accuracy")),
            ("data_rate", ConfigValue::Int(2)),
        ],
    )
    .unwrap();
    assert_eq!(mag.read(), Err(Status::AGAIN));
    mag.read().unwrap();
    assert_eq!(sim.byte(ADDRESS, reg(REG_REP_XY)), Some(0x17));
    assert_eq!(sim.byte(ADDRESS, reg(REG_REP_Z)), Some(0x52));
    // 2 Hz (0b001) in bits 5-3.
    assert_eq!(sim.byte(ADDRESS, reg(REG_OP_MODE)), Some(0x08));
}

#[test]
fn refuses_bad_config() {
    let (mut board, _sim) = chip();
    let cases: [&[(&str, ConfigValue<'_>)]; 5] = [
        &[("address", ConfigValue::Int(0x80))],
        &[("preset", ConfigValue::Str("bogus"))],
        &[("preset", ConfigValue::Int(1))],
        &[("data_rate", ConfigValue::Int(7))],
        &[("data_rate", ConfigValue::Str("10"))],
    ];
    for entries in cases {
        assert_eq!(
            bind(&mut board, entries).err(),
            Some(Status::INVALID_ARGUMENT)
        );
    }
}

#[test]
fn missing_bus_is_refused() {
    let mut board = SimBoard::new();
    assert_eq!(bind(&mut board, &[]).err(), Some(Status::NOT_FOUND));
}

#[test]
fn reads_tesla_after_start_up() {
    let (mut board, sim) = chip();
    sim.set_bytes(ADDRESS, reg(REG_DATA), &data(160, -320, 1000, XYZ1));
    let mut mag = bind(&mut board, &[]).unwrap();
    assert_eq!(mag.read(), Err(Status::AGAIN));

    let field = mag.read().unwrap();
    // Neutral X/Y trim at rhall = xyz1: 256 × 160 / 8192 / 16 = 0.3125 µT per LSB.
    close_ut(field.x, 50.0);
    close_ut(field.y, -100.0);
    // With rhall = xyz1 and z3 = z4 = 0 the Z formula reduces to
    // raw × 2048 / (z2 + z1 × rhall / 32768) in µT.
    let gain = f32::from(Z2) + f32::from(Z1) * f32::from(XYZ1) / 32768.0;
    close_ut(field.z, 1000.0 * 2048.0 / gain);
}

#[test]
fn overflowed_axes_read_as_nan() {
    let (mut board, sim) = chip();
    let bytes = data(RawSample::XY_OVERFLOW, 10, RawSample::Z_OVERFLOW, XYZ1);
    sim.set_bytes(ADDRESS, reg(REG_DATA), &bytes);
    let mut mag = bind(&mut board, &[]).unwrap();
    assert_eq!(mag.read(), Err(Status::AGAIN));
    let field = mag.read().unwrap();
    assert!(field.x.is_nan());
    assert!(field.y.is_finite());
    assert!(field.z.is_nan());
}

#[test]
fn wrong_chip_is_refused_then_retried() {
    let (mut board, sim) = chip();
    sim.set_bytes(ADDRESS, reg(REG_CHIP_ID), &[0x00]);
    sim.set_bytes(ADDRESS, reg(REG_DATA), &data(0, 0, 0, XYZ1));
    let mut mag = bind(&mut board, &[]).unwrap();
    assert_eq!(mag.read(), Err(Status::AGAIN));
    assert_eq!(mag.read(), Err(Status::NOT_SUPPORTED));
    assert_eq!(Driver::health(&mag), Health::Failed("chip ID mismatch"));

    sim.set_bytes(ADDRESS, reg(REG_CHIP_ID), &[CHIP_ID]);
    assert_eq!(mag.read(), Err(Status::AGAIN));
    assert!(mag.read().is_ok());
    assert_eq!(Driver::health(&mag), Health::Ok);
}

#[test]
fn bus_fault_powers_up_again_on_the_next_read() {
    let (mut board, sim) = chip();
    sim.set_bytes(ADDRESS, reg(REG_DATA), &data(160, -320, 1000, XYZ1));
    let mut mag = bind(&mut board, &[]).unwrap();
    assert_eq!(mag.read(), Err(Status::AGAIN));
    mag.read().unwrap();

    sim.set_dead(ADDRESS, true);
    assert_eq!(mag.read(), Err(Status::IO));
    assert_eq!(Driver::health(&mag), Health::Failed("bus error"));

    sim.set_dead(ADDRESS, false);
    assert_eq!(mag.read(), Err(Status::AGAIN));
    assert!(mag.read().is_ok());
}

#[test]
fn stale_read_before_any_conversion_is_again() {
    let (mut board, sim) = chip();
    sim.set_bytes(ADDRESS, reg(REG_DATA), &stale(160, -320, 1000, XYZ1));
    let mut mag = bind(&mut board, &[]).unwrap();
    assert_eq!(mag.read(), Err(Status::AGAIN));
    assert_eq!(mag.read(), Err(Status::AGAIN));
}

#[test]
fn stale_read_repeats_the_last_conversion() {
    let (mut board, sim) = chip();
    sim.set_bytes(ADDRESS, reg(REG_DATA), &data(160, -320, 1000, XYZ1));
    let mut mag = bind(&mut board, &[]).unwrap();
    assert_eq!(mag.read(), Err(Status::AGAIN));
    let ready = mag.read().unwrap();

    // Nothing new converted: the same field again, until a new conversion.
    sim.set_bytes(ADDRESS, reg(REG_DATA), &stale(-7, 9, -11, XYZ1 + 3));
    assert_eq!(mag.read(), Ok(ready));
    assert_eq!(mag.read(), Ok(ready));

    sim.set_bytes(ADDRESS, reg(REG_DATA), &data(-7, 9, -11, XYZ1 + 3));
    assert_ne!(mag.read().unwrap(), ready);
}

#[test]
fn describes_three_axes_in_tesla() {
    let (mut board, _sim) = chip();
    let mag = bind(&mut board, &[]).unwrap();
    let mut out = Description::new();
    Driver::describe(&mag, &mut out);
    // Three axes, and the self-test's action.
    assert_eq!(out.items().count(), 4);
    assert_eq!(out.find("self_test").unwrap().kind, ItemKind::Action);
    let item = out.find("magnetic_field.y").unwrap();
    assert_eq!(item.unit, Unit::Tesla);
    assert_eq!(item.field, Some("y"));
}

#[test]
fn fixed_read_matches_known_values() {
    let field = trim().compensate_fixed(RawSample::from_registers(data(160, -320, 1000, XYZ1)));
    // 50 µT and -100 µT exactly, in 1/16 µT.
    assert_eq!(field.x_ut16, Some(800));
    assert_eq!(field.y_ut16, Some(-1600));
    let overflow = Trim::default().compensate_fixed(RawSample {
        x: RawSample::XY_OVERFLOW,
        z: RawSample::Z_OVERFLOW,
        ..RawSample::default()
    });
    assert_eq!((overflow.x_ut16, overflow.z_ut16), (None, None));
}

/// A small deterministic generator, so the sweep needs no dependency.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }

    fn range(&mut self, low: i32, high: i32) -> i32 {
        low + (self.next() % (high - low + 1) as u32) as i32
    }
}

#[test]
fn compensation_does_not_overflow_over_trim_ranges() {
    // Debug builds panic on i32 overflow, so this sweep checks every
    // intermediate across the trim and sample ranges the chip reports.
    let mut rng = Lcg(0x5eed);
    for _ in 0..20_000 {
        let trim = Trim {
            x1: rng.range(-10, 10) as i8,
            y1: rng.range(-10, 10) as i8,
            x2: rng.range(-30, 30) as i8,
            y2: rng.range(-30, 30) as i8,
            z1: rng.range(20_000, 26_000) as u16,
            z2: rng.range(400, 900) as i16,
            z3: rng.range(-300, 300) as i16,
            z4: rng.range(-50, 50) as i16,
            xy1: rng.range(0, 40) as u8,
            xy2: rng.range(-10, 10) as i8,
            xyz1: rng.range(6_000, 7_500) as u16,
        };
        let raw = RawSample {
            x: rng.range(-4095, 4095) as i16,
            y: rng.range(-4095, 4095) as i16,
            z: rng.range(-16383, 16383) as i16,
            rhall: rng.range(5_000, 8_000) as u16,
            data_ready: true,
        };
        let vector = trim.compensate_fixed(raw).to_vector();
        for axis in [vector.x, vector.y, vector.z] {
            assert!(axis.is_nan() || axis.is_finite());
        }
    }
}

/// Runs the identity test to its answer on the driver's own tick, and reads the answer back.
fn run_identity(mag: &mut Bmm150) -> (JobState, Option<Record>) {
    let job = SelfTest::run(mag, "identity", false).unwrap();
    Driver::tick(mag, 0);
    let mut buffer = [0u8; lemnos_contract_self_test::RECORD_BYTES + 8];
    let mut writer = Writer::new(&mut buffer);
    let state = Lifecycle::job(mag, job, &mut writer);
    let used = writer.len();
    let record = match state {
        JobState::Finished(Status::OK) => {
            Some(Record::read(&mut Reader::new(&buffer[..used])).unwrap())
        }
        _ => None,
    };
    (state, record)
}

#[test]
fn the_identity_test_is_listed_and_passes_on_the_chip() {
    let (mut board, _sim) = chip();
    let mut mag = bind(&mut board, &[]).unwrap();
    let mut listed = [0u8; 512];
    let used = SelfTest::tests(&mut mag, &mut listed).unwrap();
    let mut names = read_tests(&listed[..used]).unwrap();
    assert_eq!(names.next().unwrap().name.as_str(), "identity");
    assert!(names.next().is_none(), "one test");
    let (state, record) = run_identity(&mut mag);
    assert_eq!(state, JobState::Finished(Status::OK));
    let record = record.unwrap();
    assert_eq!(record.outcome, Outcome::Pass);
    assert_eq!(record.reason, reason::OK);
}

#[test]
fn the_identity_test_reports_another_chip_as_a_failure_with_what_it_read() {
    let (mut board, sim) = chip();
    let mut mag = bind(&mut board, &[]).unwrap();
    sim.set_bytes(ADDRESS, reg(REG_CHIP_ID), &[0x40]);
    let (_, record) = run_identity(&mut mag);
    let record = record.unwrap();
    assert_eq!(record.outcome, Outcome::Fail);
    assert_eq!(record.reason, reason::IDENTITY);
    assert_eq!(record.measures().next().unwrap().value, 0x40);
}

#[test]
fn the_identity_test_reports_a_dead_chip_as_no_response_and_leaves_the_driver_alone() {
    let (mut board, sim) = chip();
    let mut mag = bind(&mut board, &[]).unwrap();
    let before = Driver::health(&mag);
    sim.set_dead(ADDRESS, true);
    let (_, record) = run_identity(&mut mag);
    let record = record.unwrap();
    assert_eq!(record.reason, reason::NO_RESPONSE);
    assert_eq!(
        Driver::health(&mag),
        before,
        "a failed test read is not a fault of the driver"
    );
}
