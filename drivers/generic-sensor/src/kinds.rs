//! The description form for each contract other than `magnetometer/1` (which `tests.rs` covers),
//! on a simulated bus, with the chip files under `tests/chips/`.

use super::*;
use description::{Kind, ParseError};
use lemnos_core::{Description, Driver, Resources};
use lemnos_measure::test_alloc::allocations;
use lemnos_sim::{Layout, SimBoard, SimI2c};

const ADDRESS: u8 = 0x40;

/// A chip file: its text without `std`, its path with `std`.
/// A chip file's text: the `description` setting in every build.
macro_rules! chip_file {
    ($name:literal) => {
        include_str!(concat!("../tests/chips/", $name))
    };
}

const ACCEL: &str = chip_file!("sim-accel.chip");
const GYRO: &str = chip_file!("sim-gyro.chip");
const TEMP: &str = chip_file!("sim-temp.chip");
const TEMP_OFFSET: &str = chip_file!("sim-humid-temp.chip");
const POWER: &str = chip_file!("sim-power.chip");

fn bus() -> (SimBoard, SimI2c) {
    let board = SimBoard::new();
    let sim = board.add_i2c("bus");
    sim.attach(ADDRESS, Layout::Bytes);
    (board, sim)
}

/// Binds driver `D` with a chip file at `ADDRESS`.
fn bind<D: Driver>(board: &mut SimBoard, description: &str) -> Result<D> {
    let entries = [
        ("address", ConfigValue::Int(i64::from(ADDRESS))),
        ("description", ConfigValue::Str(description)),
    ];
    let mut res = Resources::new(board, None);
    D::bind(&mut res, &Config::new(&entries))
}

fn close(actual: f32, expected: f32) {
    let tolerance = expected.abs() * 1e-5 + 1e-9;
    assert!(
        (actual - expected).abs() <= tolerance,
        "{actual} != {expected}"
    );
}

/// A 12-bit count left-justified in 16 bits, as the accelerometer file's chip holds it.
fn left12(count: i16) -> [u8; 2] {
    (count << 4).to_le_bytes()
}

#[test]
fn an_accelerometer_from_a_file_reads_left_justified_counts() {
    let (mut board, sim) = bus();
    sim.set_bytes(ADDRESS, 0x0f, &[0x33]);
    sim.set_bytes(ADDRESS, 0x28, &left12(-1000));
    sim.set_bytes(ADDRESS, 0x2a, &left12(2047));
    sim.set_bytes(ADDRESS, 0x2c, &left12(-2048));
    let mut accel = bind::<GenericAccelerometer>(&mut board, ACCEL).unwrap();

    let value = accel.read().unwrap();
    assert_eq!(sim.byte(ADDRESS, 0x20), Some(0x57), "the start-up write");
    close(value.x, -1000.0 * 0.0095768);
    close(value.y, 2047.0 * 0.0095768);
    close(value.z, -2048.0 * 0.0095768);
    assert_eq!(Driver::health(&accel), Health::Ok);

    // The low four bits are not the count.
    sim.set_bytes(ADDRESS, 0x28, &[0x0f | (5 << 4), 0x00]);
    close(accel.read().unwrap().x, 5.0 * 0.0095768);

    // No FIFO in a description: no batches.
    let mut into = [0u8; 40];
    assert_eq!(accel.read_batch(&mut into), Err(Status::NOT_SUPPORTED));

    let mut out = Description::new();
    Driver::describe(&accel, &mut out);
    assert_eq!(out.items().count(), 3);
    let item = out.find("acceleration.z").unwrap();
    assert_eq!(item.unit, Unit::MetrePerSecondSquared);
    assert_eq!(item.field, Some("z"));
}

#[test]
fn an_accelerometer_with_the_wrong_identity_is_refused_then_checked_again() {
    let (mut board, sim) = bus();
    sim.set_bytes(ADDRESS, 0x0f, &[0x34]);
    let mut accel = bind::<GenericAccelerometer>(&mut board, ACCEL).unwrap();
    assert_eq!(accel.read(), Err(Status::NOT_SUPPORTED));
    assert_eq!(Driver::health(&accel), Health::Failed("identity mismatch"));
    assert_eq!(
        sim.byte(ADDRESS, 0x20),
        None,
        "no write before the identity"
    );
    sim.set_bytes(ADDRESS, 0x0f, &[0x33]);
    assert!(accel.read().is_ok());
}

#[test]
fn a_gyroscope_from_a_file_waits_out_its_reset_then_reads() {
    let (mut board, sim) = bus();
    sim.set_bytes(ADDRESS, 0x00, &[0x0f]);
    sim.set_bytes(ADDRESS, 0x0f, &[0xff]);
    sim.set_bytes(ADDRESS, 0x02, &[0x10, 0x27, 0xf0, 0xd8, 0x00, 0x00]);
    let mut gyro = bind::<GenericGyroscope>(&mut board, GYRO).unwrap();

    assert_eq!(gyro.read(), Err(Status::AGAIN));
    assert_eq!(sim.byte(ADDRESS, 0x7e), Some(0xb6));
    assert_eq!(sim.byte(ADDRESS, 0x0f), Some(0xff), "held by the wait");
    assert_eq!(Driver::health(&gyro), Health::Degraded("starting"));
    assert_eq!(Driver::tick(&mut gyro, 29_000_000), Some(30_000_000));
    assert_eq!(gyro.read(), Err(Status::AGAIN));
    assert_eq!(Driver::tick(&mut gyro, 30_000_000), None);

    let rate = gyro.read().unwrap();
    assert_eq!(sim.byte(ADDRESS, 0x0f), Some(0x00));
    close(rate.x, 10_000.0 * 0.00106526);
    close(rate.y, -10_000.0 * 0.00106526);
    close(rate.z, 0.0);
    let mut into = [0u8; 40];
    assert_eq!(gyro.read_batch(&mut into), Err(Status::NOT_SUPPORTED));

    let mut out = Description::new();
    Driver::describe(&gyro, &mut out);
    assert_eq!(
        out.find("angular_rate.x").unwrap().unit,
        Unit::RadianPerSecond
    );
}

#[test]
fn a_temperature_sensor_from_a_file_writes_a_wide_init_and_reads_celsius() {
    let (mut board, sim) = bus();
    sim.set_bytes(ADDRESS, 0x00, &((-401i16) << 4).to_be_bytes());
    let mut temp = bind::<GenericTemperature>(&mut board, TEMP).unwrap();
    close(temp.read().unwrap(), -25.0625);
    assert_eq!(
        (sim.byte(ADDRESS, 0x04), sim.byte(ADDRESS, 0x05)),
        (Some(0x60), Some(0xa0)),
        "the 16-bit start-up write, high byte first"
    );
    sim.set_bytes(ADDRESS, 0x00, &(1200i16 << 4).to_be_bytes());
    close(temp.read().unwrap(), 75.0);

    let mut out = Description::new();
    Driver::describe(&temp, &mut out);
    assert_eq!(out.items().count(), 1);
    let item = out.find("temperature").unwrap();
    assert_eq!(item.unit, Unit::DegreeCelsius);
    assert_eq!(item.field, None);
}

#[test]
fn a_temperature_with_an_offset_reads_count_times_scale_plus_offset() {
    let (mut board, sim) = bus();
    sim.set_bytes(ADDRESS, 0x05, &6_500u16.to_le_bytes());
    let mut temp = bind::<GenericTemperature>(&mut board, TEMP_OFFSET).unwrap();
    close(temp.read().unwrap(), 25.0);
    sim.set_bytes(ADDRESS, 0x05, &0u16.to_le_bytes());
    close(temp.read().unwrap(), -40.0);
}

/// The power monitor file's chip, with its die ID and the four readings as counts.
fn power_chip(sim: &SimI2c, shunt: i16, bus_v: u16, power: u16, current: i16) {
    sim.set_bytes(ADDRESS, 0xfe, &[0x22]);
    sim.set_bytes(ADDRESS, 0x02, &shunt.to_be_bytes());
    sim.set_bytes(ADDRESS, 0x04, &bus_v.to_be_bytes());
    sim.set_bytes(ADDRESS, 0x06, &power.to_be_bytes());
    sim.set_bytes(ADDRESS, 0x08, &current.to_be_bytes());
}

#[test]
fn a_power_monitor_from_a_file_calibrates_and_reads_all_four() {
    let (mut board, sim) = bus();
    power_chip(&sim, -400, 9_600, 480, -1_000);
    let mut power = bind::<GenericPowerMonitor>(&mut board, POWER).unwrap();
    let reading = power.read().unwrap();
    assert_eq!(
        (sim.byte(ADDRESS, 0x00), sim.byte(ADDRESS, 0x01)),
        (Some(0x41), Some(0x27)),
        "configuration"
    );
    assert_eq!(
        (sim.byte(ADDRESS, 0x0a), sim.byte(ADDRESS, 0x0b)),
        (Some(0x02), Some(0x00)),
        "calibration"
    );
    close(reading.shunt_v, -0.001);
    close(reading.bus_v, 12.0);
    close(reading.power_w, 12.0);
    close(reading.current_a, -1.0);

    power_chip(&sim, 400, 4_000, 100, 250);
    let reading = power.read().unwrap();
    close(reading.shunt_v, 0.001);
    close(reading.bus_v, 5.0);
    close(reading.power_w, 2.5);
    close(reading.current_a, 0.25);

    let mut out = Description::new();
    Driver::describe(&power, &mut out);
    assert_eq!(out.items().count(), 4);
    assert_eq!(out.find("current").unwrap().unit, Unit::Ampere);
    assert_eq!(out.find("power").unwrap().field, Some("power_w"));
}

#[test]
fn a_power_monitor_bus_fault_is_reported_and_tried_again() {
    let (mut board, sim) = bus();
    power_chip(&sim, 0, 0, 0, 0);
    let mut power = bind::<GenericPowerMonitor>(&mut board, POWER).unwrap();
    power.read().unwrap();
    sim.set_dead(ADDRESS, true);
    for _ in 0..10 {
        assert_eq!(power.read(), Err(Status::IO));
        assert_eq!(Driver::health(&power), Health::Failed("bus error"));
    }
    sim.set_dead(ADDRESS, false);
    sim.set_bytes(ADDRESS, 0x0a, &[0, 0]);
    assert!(power.read().is_ok());
    assert_eq!(sim.byte(ADDRESS, 0x0a), Some(0x02), "started again");
    assert_eq!(Driver::health(&power), Health::Ok);
}

#[test]
fn steady_state_reads_of_every_kind_allocate_nothing() {
    let (mut board, sim) = bus();
    power_chip(&sim, 1, 2, 3, 4);
    let mut power = bind::<GenericPowerMonitor>(&mut board, POWER).unwrap();
    power.read().unwrap();
    let (mut board, sim) = bus();
    sim.set_bytes(ADDRESS, 0x00, &[0, 0]);
    let mut temp = bind::<GenericTemperature>(&mut board, TEMP).unwrap();
    temp.read().unwrap();
    let ((), allocated) = allocations(|| {
        for _ in 0..1000 {
            power.read().unwrap();
            temp.read().unwrap();
        }
    });
    assert_eq!(allocated, 0);
}

#[test]
fn a_file_for_one_contract_is_refused_by_another() {
    let (mut board, _sim) = bus();
    for (text, ok) in [(TEMP, false), (POWER, false), (ACCEL, true)] {
        assert_eq!(
            bind::<GenericGyroscope>(&mut board, text).is_ok(),
            ok,
            "{text}"
        );
    }
    assert_eq!(
        bind::<GenericTemperature>(&mut board, POWER).err(),
        Some(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        bind::<GenericPowerMonitor>(&mut board, TEMP).err(),
        Some(Status::INVALID_ARGUMENT)
    );
}

#[test]
fn value_lines_and_their_options_are_checked() {
    let error = |text: &str, kind| Chip::parse(text, kind).err();
    let at = |line, reason| Some(ParseError { line, reason });
    let t = Kind::Temperature;
    assert_eq!(
        error("value humidity 0 8 big unsigned 1", t),
        at(1, "not a value this contract reads")
    );
    assert_eq!(
        error("axis x 0 8 big unsigned 1", t),
        at(1, "this contract takes value lines, not axis lines")
    );
    assert_eq!(
        error("value temperature 0 8 big unsigned 1", Kind::Accelerometer),
        at(1, "this contract takes axis lines, not value lines")
    );
    assert_eq!(
        error("value temperature 0 30 big signed 1 shift 3", t),
        at(1, "bits and shift together are more than 32")
    );
    assert_eq!(
        error("value temperature 0 0 big signed 1", t),
        at(1, "bits is 1 to 32")
    );
    assert_eq!(
        error("value temperature 0 12 big signed 1 shift 4 shift 4", t),
        at(1, "an option is given twice")
    );
    assert_eq!(
        error("value temperature 0 12 big signed 1 offset inf", t),
        at(1, "scale or offset is not a finite number")
    );
    assert_eq!(
        error("value temperature 0 12 big signed 1 gain 2", t),
        at(1, "extra text at the end of the line")
    );
    assert_eq!(
        error(
            "value temperature 0 8 big signed 1\nvalue temperature 0 8 big signed 1",
            t
        ),
        at(2, "value given twice")
    );
    assert_eq!(
        error("init 0x01 0x1ff", t),
        at(1, "a register or value is wider than its bits")
    );
    assert_eq!(
        error("init 0x01 0x10000 16 big", t),
        at(1, "a register or value is wider than its bits")
    );
    assert_eq!(
        error("init 0x01 0x1 12 big", t),
        at(1, "an init's bits is 8, 16, 24 or 32")
    );
    assert_eq!(
        error("init 0x01 0x1 16 middle", t),
        at(1, "endian is little or big")
    );
    assert_eq!(error("", t), at(0, "a value the contract reads is missing"));
    assert_eq!(
        error(
            "value bus_v 0 8 big signed 1\nvalue shunt_v 0 8 big signed 1\nvalue power_w 0 8 big signed 1",
            Kind::PowerMonitor
        ),
        at(0, "a value the contract reads is missing")
    );
}

#[test]
fn a_channel_reads_as_many_bytes_as_its_bits_and_shift_fill() {
    let chip = Chip::parse(
        "value temperature 0 13 big unsigned 1 shift 3",
        Kind::Temperature,
    )
    .unwrap();
    let channel = chip.channels()[0];
    assert_eq!((channel.bytes, channel.bits, channel.shift), (2, 13, 3));
    // The low three bits are flags, not the count.
    assert_eq!(channel.units(0xffff), 8191.0);
    assert_eq!(channel.units(0x0007), 0.0);

    let chip = Chip::parse(
        "value temperature 0 12 big signed 0.5 offset 10",
        Kind::Temperature,
    )
    .unwrap();
    let channel = chip.channels()[0];
    assert_eq!(channel.bytes, 2);
    // The top four bits are not the count; bit 11 is its sign.
    assert_eq!(channel.units(0xf800), 10.0 - 1024.0);
    assert_eq!(channel.units(0x07ff), 10.0 + 1023.5);

    let chip = Chip::parse(
        "value temperature 0 1 big unsigned 1 shift 7",
        Kind::Temperature,
    )
    .unwrap();
    assert_eq!(chip.channels()[0].bytes, 1);
    assert_eq!(chip.channels()[0].units(0x80), 1.0);
}
