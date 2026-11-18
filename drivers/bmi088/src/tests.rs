//! The driver on a simulated bus: each die's registers are the sim's bytes, and
//! each die's FIFO is the sim's FIFO.

extern crate std;

use super::*;
use crate::clock_tests::{Harness, Jitter, Oscillator, Plan};
use chip::{Frame, scan_accel};
use lemnos_contract_accelerometer::{RECORD_LEN as ACCEL_RECORD, Sample as AccelSample};
use lemnos_contract_gyroscope::{RECORD_LEN as GYRO_RECORD, Sample as GyroSample};
use lemnos_core::{
    Config, Decode, Description, Driver, Reader, RefusalKind, RefusalSink, Resources, Writer,
};
use lemnos_sim::{FifoLevel, Layout, SimBoard, SimI2c};
use std::string::ToString;
use std::time::Duration;
use std::vec::Vec;

const ACCEL: u8 = DEFAULT_ACCEL_ADDRESS;
const GYRO: u8 = DEFAULT_GYRO_ADDRESS;
const G: f32 = 9.806_65;
const DPS: f32 = core::f32::consts::PI / 180.0;

const ACC_ID: u8 = 0x00;
const ACC_DATA: u8 = 0x12;
const ACC_CONF: u8 = 0x40;
const ACC_RANGE: u8 = 0x41;
const ACC_FIFO_LENGTH: u8 = 0x24;
const ACC_FIFO_DATA: u8 = 0x26;
const ACC_FIFO_DOWNS: u8 = 0x45;
const ACC_FIFO_CONFIG_0: u8 = 0x48;
const ACC_FIFO_CONFIG_1: u8 = 0x49;
const ACC_PWR_CONF: u8 = 0x7c;
const ACC_PWR_CTRL: u8 = 0x7d;
const ACC_SOFTRESET: u8 = 0x7e;

const GYR_ID: u8 = 0x00;
const GYR_DATA: u8 = 0x02;
const GYR_FIFO_STATUS: u8 = 0x0e;
const GYR_RANGE: u8 = 0x0f;
const GYR_BANDWIDTH: u8 = 0x10;
const GYR_SOFTRESET: u8 = 0x14;
const GYR_FIFO_CONFIG_0: u8 = 0x3d;
const GYR_FIFO_CONFIG_1: u8 = 0x3e;
const GYR_FIFO_DATA: u8 = 0x3f;

/// Little-endian counts as the six data-register bytes.
fn axes_bytes(x: i16, y: i16, z: i16) -> [u8; 6] {
    let (x, y, z) = (x.to_le_bytes(), y.to_le_bytes(), z.to_le_bytes());
    [x[0], x[1], y[0], y[1], z[0], z[1]]
}

/// An accelerometer FIFO data frame.
fn frame(x: i16, y: i16, z: i16) -> Vec<u8> {
    let mut out = std::vec![0x84];
    out.extend_from_slice(&axes_bytes(x, y, z));
    out
}

/// A gyroscope FIFO frame (no header).
fn gyro_frame(x: i16, y: i16, z: i16) -> Vec<u8> {
    axes_bytes(x, y, z).to_vec()
}

/// Both chips at their default addresses, with their IDs in place.
fn chip() -> (SimBoard, SimI2c) {
    let board = SimBoard::new();
    let sim = board.add_i2c("bus");
    sim.attach(ACCEL, Layout::Bytes);
    sim.set_bytes(ACCEL, ACC_ID, &[0x1e]);
    sim.attach(GYRO, Layout::Bytes);
    sim.set_bytes(GYRO, GYR_ID, &[0x0f]);
    (board, sim)
}

fn bind(board: &mut SimBoard, entries: &[(&str, ConfigValue<'_>)]) -> Result<Bmi088> {
    let mut res = Resources::new(board, None);
    Bmi088::bind(&mut res, &Config::new(entries))
}

fn close(actual: f32, expected: f32, tolerance: f32) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{actual} != {expected} (±{tolerance})"
    );
}

/// Ticks the driver at 1 ms steps until the accelerometer has a sample, and returns it. The
/// tick steps the power-up, as the host's ticks do; a read says `AGAIN` until then, or the failure
/// of a step that is being retried.
fn accel_up(dev: &mut Bmi088) -> Vector3 {
    for step in 0..10_000u64 {
        Lifecycle::tick(dev, step * 1_000_000);
        match Accelerometer::read(dev) {
            Ok(sample) => return sample,
            Err(status) => assert!(
                status == Status::AGAIN || status == Status::IO,
                "{status:?}"
            ),
        }
    }
    panic!("accelerometer never became ready");
}

/// Ticks the driver at 1 ms steps until the gyroscope has a sample, and returns it.
fn gyro_up(dev: &mut Bmi088) -> Vector3 {
    for step in 0..10_000u64 {
        Lifecycle::tick(dev, step * 1_000_000);
        match Gyroscope::read(dev) {
            Ok(sample) => return sample,
            Err(status) => assert!(
                status == Status::AGAIN || status == Status::IO,
                "{status:?}"
            ),
        }
    }
    panic!("gyroscope never became ready");
}

#[test]
fn the_manifest_provides_both_contracts() {
    assert_eq!(Bmi088::MANIFEST.provides.len(), 2);
    assert_eq!(Bmi088::MANIFEST.provides[0].tag.as_str(), "accelerometer/1");
    assert_eq!(Bmi088::MANIFEST.provides[1].tag.as_str(), "gyroscope/1");
}

#[test]
fn accelerometer_powers_up_one_step_per_tick_at_the_chips_waits() {
    let (mut board, sim) = chip();
    sim.set_bytes(ACCEL, ACC_DATA, &axes_bytes(16384, 0, 0));
    let mut dev = bind(&mut board, &[]).unwrap();
    assert_eq!(
        Driver::health(&dev),
        Health::Degraded("accelerometer: waiting for its first power-up step")
    );
    assert_eq!(Accelerometer::read(&mut dev), Err(Status::AGAIN));
    assert_eq!(sim.byte(ACCEL, ACC_SOFTRESET), None, "a read steps nothing");

    // The first tick resets the chip and asks for the next step after its 1 ms.
    assert_eq!(Driver::tick(&mut dev, 0), Some(1_000_000));
    assert_eq!(sim.byte(ACCEL, ACC_SOFTRESET), Some(0xb6));
    assert_eq!(
        Driver::health(&dev),
        Health::Degraded("accelerometer: waiting for the chip to reset")
    );

    assert_eq!(
        Driver::tick(&mut dev, 500_000),
        Some(1_000_000),
        "not yet due"
    );
    assert_eq!(sim.byte(ACCEL, ACC_PWR_CONF), None);
    assert_eq!(Driver::tick(&mut dev, 1_000_000), Some(2_000_000));
    assert_eq!(sim.byte(ACCEL, ACC_PWR_CONF), Some(0x00));
    assert_eq!(sim.byte(ACCEL, ACC_PWR_CTRL), None);
    assert_eq!(
        Driver::health(&dev),
        Health::Degraded("accelerometer: waiting for the chip to power on")
    );

    assert_eq!(Driver::tick(&mut dev, 2_000_000), Some(7_000_000));
    assert_eq!(sim.byte(ACCEL, ACC_PWR_CTRL), Some(0x04));
    assert_eq!(sim.byte(ACCEL, ACC_RANGE), None);
    assert_eq!(Driver::tick(&mut dev, 6_999_999), Some(7_000_000));
    assert_eq!(
        sim.byte(ACCEL, ACC_RANGE),
        None,
        "the 5 ms after power-on are not over"
    );

    // The configuration step writes the settings; the gyroscope's reset is due at 30 ms.
    assert_eq!(Driver::tick(&mut dev, 7_000_000), Some(30_000_000));
    assert_eq!(sim.byte(ACCEL, ACC_RANGE), Some(0x01));
    assert_eq!(sim.byte(ACCEL, ACC_CONF), Some(0xa8));
    let sample = Accelerometer::read(&mut dev).unwrap();
    close(sample.x, 3.0 * G, 1e-3);
    assert_eq!(
        Driver::health(&dev),
        Health::Degraded("gyroscope: waiting for the chip to reset")
    );
}

#[test]
fn gyroscope_powers_up_in_steps_from_its_ticks_and_reads_in_rad_per_s() {
    let (mut board, sim) = chip();
    sim.set_bytes(GYRO, GYR_DATA, &axes_bytes(16384, -16384, 0));
    let mut dev = bind(&mut board, &[]).unwrap();

    assert_eq!(Gyroscope::read(&mut dev), Err(Status::AGAIN));
    assert_eq!(sim.byte(GYRO, GYR_SOFTRESET), None, "a read steps nothing");
    // Ticks at 1 ms steps: the reset, 30 ms of waiting, the check and then the configuration.
    let mut now = 0;
    while sim.byte(GYRO, GYR_RANGE).is_none() {
        Driver::tick(&mut dev, now);
        now += 1_000_000;
        assert!(now <= 100_000_000, "the gyroscope never configured");
    }
    assert_eq!(sim.byte(GYRO, GYR_SOFTRESET), Some(0xb6));
    assert!(
        now >= 35_000_000,
        "the 30 ms reset and the 5 ms after it: {now}"
    );

    let sample = Gyroscope::read(&mut dev).unwrap();
    // Default 2000 degrees per second: half scale is 1000.
    assert_eq!(sim.byte(GYRO, GYR_RANGE), Some(0x00));
    assert_eq!(sim.byte(GYRO, GYR_BANDWIDTH), Some(0x00));
    close(sample.x, 1000.0 * DPS, 1e-4);
    close(sample.y, -1000.0 * DPS, 1e-4);
    close(sample.z, 0.0, 1e-6);
}

/// Binds as [`bind`] does, and says what the driver gave as its refusal reason, with its kind.
fn bind_saying(board: &mut SimBoard) -> (Result<Bmi088>, Vec<(RefusalKind, std::string::String)>) {
    let reasons = std::sync::Mutex::new(Vec::new());
    let sink = |kind: RefusalKind, args: core::fmt::Arguments<'_>| {
        reasons
            .lock()
            .unwrap()
            .push((kind, std::string::ToString::to_string(&args)));
    };
    let entries: &[(&str, ConfigValue)] = &[];
    let config = Config::new(entries).with_refusals(RefusalSink::new(&sink));
    let mut res = Resources::new(board, None);
    let bound = Bmi088::bind(&mut res, &config);
    (bound, reasons.into_inner().unwrap())
}

#[test]
fn wrong_chip_is_refused_at_bind_and_nothing_is_written() {
    let (mut board, sim) = chip();
    sim.set_bytes(ACCEL, ACC_ID, &[0x00]);
    let (bound, reasons) = bind_saying(&mut board);
    assert_eq!(bound.err(), Some(Status::NOT_SUPPORTED));
    assert_eq!(
        reasons,
        [(
            RefusalKind::Failed,
            "the accelerometer at 0x18 answers ID 0x00, expected 0x1e".to_string()
        )]
    );
    assert_eq!(sim.byte(ACCEL, ACC_SOFTRESET), None);

    // Once the right chip is there, the retry binds and the first tick powers it up.
    sim.set_bytes(ACCEL, ACC_ID, &[0x1e]);
    let mut dev = bind(&mut board, &[]).unwrap();
    Driver::tick(&mut dev, 0);
    assert_eq!(sim.byte(ACCEL, ACC_SOFTRESET), Some(0xb6));
}

#[test]
fn an_accelerometer_that_does_not_acknowledge_is_absent_at_bind_then_bound_when_it_appears() {
    // No die at the address: the bus NACKs, as the real bus does for a missing device.
    let mut board = SimBoard::new();
    let sim = board.add_i2c("bus");
    let (bound, reasons) = bind_saying(&mut board);
    assert_eq!(bound.err(), Some(Status::NO_DEVICE));
    assert_eq!(
        reasons,
        [(
            RefusalKind::Absent,
            "no device at 0x18: nothing answers at its address".to_string()
        )]
    );

    // The die appears later: the next bind, a retry, finds it.
    sim.attach(ACCEL, Layout::Bytes);
    sim.set_bytes(ACCEL, ACC_ID, &[0x1e]);
    assert!(bind_saying(&mut board).0.is_ok());
}

#[test]
fn a_missing_gyroscope_does_not_stop_the_accelerometer() {
    let (mut board, sim) = chip();
    sim.set_dead(GYRO, true);
    let mut dev = bind(&mut board, &[]).unwrap();
    accel_up(&mut dev);

    // The gyroscope's ticks fail on the bus and are retried; the accelerometer still reads.
    assert_eq!(
        Driver::health(&dev),
        Health::Degraded("gyroscope: transfer failed, retrying")
    );
    assert!(Accelerometer::read(&mut dev).is_ok());

    sim.set_dead(GYRO, false);
    gyro_up(&mut dev);
}

#[test]
fn a_transfer_failure_while_measuring_restarts_power_up_on_a_later_read() {
    let (mut board, sim) = chip();
    let mut dev = bind(&mut board, &[]).unwrap();
    accel_up(&mut dev);

    sim.set_dead(ACCEL, true);
    assert_eq!(Accelerometer::read(&mut dev), Err(Status::IO));
    assert_eq!(
        Driver::health(&dev),
        Health::Degraded("accelerometer: transfer failed, retrying")
    );

    // The chip comes back: the next tick soft-resets it and starts over.
    sim.set_bytes(ACCEL, ACC_SOFTRESET, &[0]);
    sim.set_dead(ACCEL, false);
    Driver::tick(&mut dev, 0);
    assert_eq!(sim.byte(ACCEL, ACC_SOFTRESET), Some(0xb6));
    accel_up(&mut dev);
}

#[test]
fn accelerometer_reads_si_units_for_the_configured_range() {
    let (mut board, sim) = chip();
    let mut dev = bind(&mut board, &[]).unwrap();
    accel_up(&mut dev);

    // Full scale is 6 g: 16384 counts is half of it, 3 g.
    sim.set_bytes(ACCEL, ACC_DATA, &axes_bytes(16384, -8192, 32767));
    let sample = Accelerometer::read(&mut dev).unwrap();
    close(sample.x, 3.0 * G, 1e-3);
    close(sample.y, -1.5 * G, 1e-3);
    close(sample.z, 6.0 * G * 32767.0 / 32768.0, 1e-3);
}

#[test]
fn config_selects_ranges_and_rates_and_scales_to_them() {
    let (mut board, sim) = chip();
    let mut dev = bind(
        &mut board,
        &[
            ("accel_range", ConfigValue::Int(3)),
            ("accel_rate", ConfigValue::Int(200)),
            ("gyro_range", ConfigValue::Int(500)),
            ("gyro_rate", ConfigValue::Int(200)),
        ],
    )
    .unwrap();
    sim.set_bytes(ACCEL, ACC_DATA, &axes_bytes(16384, 0, 0));
    sim.set_bytes(GYRO, GYR_DATA, &axes_bytes(16384, 0, 0));
    accel_up(&mut dev);
    let gyro = gyro_up(&mut dev);

    assert_eq!(sim.byte(ACCEL, ACC_RANGE), Some(0x00));
    assert_eq!(sim.byte(ACCEL, ACC_CONF), Some(0xa9));
    assert_eq!(sim.byte(GYRO, GYR_RANGE), Some(0x02));
    assert_eq!(sim.byte(GYRO, GYR_BANDWIDTH), Some(0x04));
    close(gyro.x, 250.0 * DPS, 1e-4);
    let sample = Accelerometer::read(&mut dev).unwrap();
    close(sample.x, 1.5 * G, 1e-3);
}

#[test]
fn fractional_accelerometer_rate_is_accepted() {
    let (mut board, sim) = chip();
    let mut dev = bind(&mut board, &[("accel_rate", ConfigValue::Float(12.5))]).unwrap();
    accel_up(&mut dev);
    assert_eq!(sim.byte(ACCEL, ACC_CONF), Some(0xa5));
}

#[test]
fn config_refuses_values_it_cannot_set() {
    let (mut board, _sim) = chip();
    for entries in [
        std::vec![("accel_range", ConfigValue::Int(5))],
        std::vec![("accel_rate", ConfigValue::Int(150))],
        std::vec![("gyro_range", ConfigValue::Int(4000))],
        std::vec![("gyro_rate", ConfigValue::Int(150))],
        std::vec![("accel_address", ConfigValue::Int(0x80))],
        std::vec![("gyro_address", ConfigValue::Str("0x68"))],
        std::vec![("fifo", ConfigValue::Int(1))],
    ] {
        assert_eq!(
            bind(&mut board, &entries).err(),
            Some(Status::INVALID_ARGUMENT),
            "{entries:?}"
        );
    }
}

#[test]
fn addresses_come_from_the_config() {
    let mut board = SimBoard::new();
    let sim = board.add_i2c("bus");
    sim.attach(0x19, Layout::Bytes);
    sim.set_bytes(0x19, ACC_ID, &[0x1e]);
    sim.attach(0x6a, Layout::Bytes);
    sim.set_bytes(0x6a, GYR_ID, &[0x0f]);
    let mut dev = bind(
        &mut board,
        &[
            ("accel_address", ConfigValue::Int(0x19)),
            ("gyro_address", ConfigValue::Int(0x6a)),
        ],
    )
    .unwrap();
    Driver::tick(&mut dev, 0);
    assert_eq!(sim.byte(0x19, ACC_SOFTRESET), Some(0xb6));
    assert_eq!(Gyroscope::read(&mut dev), Err(Status::AGAIN));
    assert_eq!(sim.byte(0x6a, GYR_SOFTRESET), Some(0xb6));
}

#[test]
fn describes_both_dies_in_si_units() {
    let (mut board, _sim) = chip();
    let dev = bind(&mut board, &[]).unwrap();
    let mut out = Description::new();
    Driver::describe(&dev, &mut out);
    assert_eq!(out.items().count(), 6);
    let accel = out.find("acceleration.x").unwrap();
    assert_eq!(accel.unit, Unit::MetrePerSecondSquared);
    assert_eq!(accel.method, ACCEL_READ);
    let gyro = out.find("angular_rate.z").unwrap();
    assert_eq!(gyro.unit, Unit::RadianPerSecond);
    assert_eq!(gyro.method, GYRO_READ);
    assert_eq!(gyro.field, Some("z"));
}

#[test]
fn fifo_power_up_starts_both_fifos_in_stream_mode() {
    let (mut board, sim) = chip();
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
    let mut dev = bind(&mut board, &[("fifo", ConfigValue::Bool(true))]).unwrap();
    sim.fifo_push(ACCEL, &frame(1, 2, 3));
    sim.fifo_push(GYRO, &gyro_frame(1, 2, 3));
    accel_up(&mut dev);
    gyro_up(&mut dev);

    assert_eq!(sim.byte(ACCEL, ACC_FIFO_DOWNS), Some(0x80));
    assert_eq!(sim.byte(ACCEL, ACC_FIFO_CONFIG_0), Some(0x02));
    assert_eq!(sim.byte(ACCEL, ACC_FIFO_CONFIG_1), Some(0x50));
    assert_eq!(sim.byte(GYRO, GYR_FIFO_CONFIG_0), Some(0x00));
    assert_eq!(sim.byte(GYRO, GYR_FIFO_CONFIG_1), Some(0x80));
}

#[test]
fn fifo_off_leaves_the_fifo_registers_alone() {
    let (mut board, sim) = chip();
    let mut dev = bind(&mut board, &[]).unwrap();
    accel_up(&mut dev);
    gyro_up(&mut dev);
    assert_eq!(sim.byte(ACCEL, ACC_FIFO_CONFIG_0), None);
    assert_eq!(sim.byte(GYRO, GYR_FIFO_CONFIG_1), None);
}

/// A chip with both FIFOs, and a device with `fifo` on.
fn fifo_chip() -> (SimBoard, SimI2c, Bmi088) {
    let (mut board, sim) = chip();
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
    let dev = bind(&mut board, &[("fifo", ConfigValue::Bool(true))]).unwrap();
    (board, sim, dev)
}

#[test]
fn accelerometer_fifo_read_returns_the_newest_frame_and_drains_the_fifo() {
    let (_board, sim, mut dev) = fifo_chip();
    let mut bytes = frame(1, 2, 3);
    bytes.extend(frame(4, 5, 6));
    bytes.extend(frame(7, 8, 9));
    sim.fifo_push(ACCEL, &bytes);

    let sample = accel_up(&mut dev);
    close(sample.x, 7.0 * 9.80665 * 6.0 / 32768.0, 1e-3);
    close(sample.z, 9.0 * 9.80665 * 6.0 / 32768.0, 1e-3);
    assert_eq!(sim.fifo_len(ACCEL), 0);
}

#[test]
fn accelerometer_fifo_with_nothing_new_repeats_the_last_sample() {
    let (_board, sim, mut dev) = fifo_chip();
    sim.fifo_push(ACCEL, &frame(100, 0, 0));
    let first = accel_up(&mut dev);

    assert_eq!(Accelerometer::read(&mut dev), Ok(first));
    sim.fifo_push(ACCEL, &frame(200, 0, 0));
    let next = Accelerometer::read(&mut dev).unwrap();
    close(next.x, 200.0 * 9.80665 * 6.0 / 32768.0, 1e-3);
}

#[test]
fn accelerometer_fifo_empty_before_the_first_sample_is_again() {
    let (_board, _sim, mut dev) = fifo_chip();
    for _ in 0..3 {
        assert_eq!(Accelerometer::read(&mut dev), Err(Status::AGAIN));
    }
    assert_eq!(Accelerometer::read(&mut dev), Err(Status::AGAIN));
    assert_eq!(Accelerometer::read(&mut dev), Err(Status::AGAIN));
}

#[test]
fn accelerometer_fifo_overflow_is_reported_on_that_read() {
    let (_board, sim, mut dev) = fifo_chip();
    // Four frames lost, then one kept.
    let mut bytes = std::vec![0x40, 4];
    bytes.extend(frame(9, 9, 9));
    sim.fifo_push(ACCEL, &bytes);
    accel_up(&mut dev);
    assert_eq!(
        Driver::health(&dev),
        Health::Degraded("accelerometer: FIFO overflowed, samples lost")
    );

    // The next read finds nothing new, so the loss is no longer reported.
    let kept = Accelerometer::read(&mut dev).unwrap();
    close(kept.x, 9.0 * 9.80665 * 6.0 / 32768.0, 1e-3);
    assert_ne!(
        Driver::health(&dev),
        Health::Degraded("accelerometer: FIFO overflowed, samples lost")
    );
}

#[test]
fn accel_fifo_frames_that_straddle_bursts_are_carried() {
    let (_board, sim, mut dev) = fifo_chip();
    // A time frame, then 16 frames: the 16th starts at byte 109 and ends at 116,
    // so it straddles the second 56-byte burst (56 to 111) and the third. The
    // newest sample is that straddling frame, so a lost carry would show.
    let mut bytes = std::vec![0x44, 1, 2, 3];
    for i in 1..=16i16 {
        bytes.extend(frame(i, -i, 2 * i));
    }
    assert_eq!(bytes.len(), 116);
    sim.fifo_push(ACCEL, &bytes);

    let sample = accel_up(&mut dev);
    close(sample.x, 16.0 * 9.80665 * 6.0 / 32768.0, 1e-3);
    close(sample.y, -16.0 * 9.80665 * 6.0 / 32768.0, 1e-3);
    close(sample.z, 32.0 * 9.80665 * 6.0 / 32768.0, 1e-3);
    assert_eq!(sim.fifo_len(ACCEL), 0);
}

#[test]
fn gyroscope_fifo_read_returns_the_newest_frame_and_drains_the_fifo() {
    let (_board, sim, mut dev) = fifo_chip();
    // Ten frames: more than one burst, so the drain is in two.
    let mut bytes = Vec::new();
    for i in 1..=10i16 {
        bytes.extend(gyro_frame(i * 100, 0, 0));
    }
    sim.fifo_push(GYRO, &bytes);

    let sample = gyro_up(&mut dev);
    close(sample.x, 1000.0 * 2000.0 / 32768.0 * DPS, 1e-4);
    assert_eq!(sim.fifo_len(GYRO), 0);
}

#[test]
fn gyroscope_fifo_full_reports_lost_samples() {
    let (_board, sim, mut dev) = fifo_chip();
    sim.fifo_push(ACCEL, &frame(1, 1, 1));
    let mut bytes = Vec::new();
    for _ in 0..100 {
        bytes.extend(gyro_frame(5, 5, 5));
    }
    sim.fifo_push(GYRO, &bytes);

    accel_up(&mut dev);
    gyro_up(&mut dev);
    assert_eq!(
        Driver::health(&dev),
        Health::Degraded("gyroscope: FIFO overflowed, samples lost")
    );
}

/// Scans `bytes`, returning the bytes used and the frames seen, in order.
fn scan(bytes: &[u8]) -> (usize, Vec<Frame>) {
    let mut frames = Vec::new();
    let used = scan_accel(bytes, &mut |frame| frames.push(frame));
    (used, frames)
}

#[test]
fn scan_data_frames_are_samples_in_order() {
    let mut bytes = frame(1, -2, 3);
    bytes.extend(frame(-4, 5, -6));
    let (used, frames) = scan(&bytes);
    assert_eq!(used, 14);
    assert_eq!(
        frames,
        [Frame::Sample([1, -2, 3]), Frame::Sample([-4, 5, -6])]
    );
}

#[test]
fn scan_tag_bits_are_ignored() {
    // INT1 and INT2 tags set: header 0x87 is still a data frame.
    let mut bytes = frame(7, 8, 9);
    bytes[0] |= 0x03;
    let (used, frames) = scan(&bytes);
    assert_eq!(used, 7);
    assert_eq!(frames, [Frame::Sample([7, 8, 9])]);
}

#[test]
fn scan_skip_frames_count_dropped_frames_and_produce_no_sample() {
    let mut bytes = std::vec![0x40, 5];
    bytes.extend(frame(1, 1, 1));
    bytes.extend_from_slice(&[0x40, 0xff]);
    let (used, frames) = scan(&bytes);
    assert_eq!(used, bytes.len());
    assert_eq!(
        frames,
        [
            Frame::Skipped(5),
            Frame::Sample([1, 1, 1]),
            Frame::Skipped(0xff)
        ]
    );
}

#[test]
fn scan_time_config_and_drop_frames_are_stepped_over() {
    let mut bytes = std::vec![0x44, 1, 2, 3];
    bytes.extend_from_slice(&[0x48, 0x03]);
    bytes.extend_from_slice(&[0x50, 0x00]);
    bytes.extend(frame(10, 20, 30));
    let (used, frames) = scan(&bytes);
    assert_eq!(used, bytes.len());
    assert_eq!(frames, [Frame::Sample([10, 20, 30])]);
}

#[test]
fn scan_a_frame_cut_short_is_left_for_the_next_burst() {
    let mut bytes = frame(1, 2, 3);
    bytes.extend_from_slice(&frame(4, 5, 6)[..4]);
    let (used, frames) = scan(&bytes);
    assert_eq!(used, 7);
    assert_eq!(frames, [Frame::Sample([1, 2, 3])]);
}

#[test]
fn scan_a_skip_header_cut_short_stops_the_scan() {
    let (used, frames) = scan(&[0x40]);
    assert_eq!(used, 0);
    assert!(frames.is_empty());
}

#[test]
fn scan_unknown_bytes_are_skipped_one_at_a_time() {
    let mut bytes = std::vec![0x00, 0x01];
    bytes.extend(frame(9, 9, 9));
    let (used, frames) = scan(&bytes);
    assert_eq!(used, bytes.len());
    assert_eq!(frames, [Frame::Sample([9, 9, 9])]);
}

#[test]
fn scan_an_empty_fifo_is_no_samples() {
    let (used, frames) = scan(&[]);
    assert_eq!(used, 0);
    assert!(frames.is_empty());
}

#[test]
fn settings_reject_unknown_values_and_map_known_ones() {
    assert!(chip::accel_setting(4, 100.0).is_none());
    assert!(chip::accel_setting(6, 13.0).is_none());
    assert_eq!(
        chip::accel_setting(6, 100.0).map(|s| s.scale > 0.0),
        Some(true)
    );
    assert!(chip::gyro_setting(2000, 150).is_none());
    assert_eq!(
        chip::gyro_setting(2000, 2000).map(|s| s.scale > 0.0),
        Some(true)
    );
}

/// A register bus whose registers hold what was written, except that `stuck`
/// always reads back `stuck_value`: a chip that has not taken the write yet.
struct Stuck {
    stuck: u16,
    stuck_value: u8,
    held: [u8; 256],
}

impl lemnos_hal::register::RegisterBus for Stuck {
    type BusError = lemnos_hal::ErrorKind;

    fn read_burst(
        &mut self,
        address: u16,
        buf: &mut [u8],
    ) -> lemnos_hal::register::RegisterResult<(), Self::BusError> {
        for (i, byte) in buf.iter_mut().enumerate() {
            let at = address + i as u16;
            *byte = if at == self.stuck {
                self.stuck_value
            } else {
                self.held[at as usize]
            };
        }
        Ok(())
    }

    fn write_burst(
        &mut self,
        address: u16,
        data: &[u8],
    ) -> lemnos_hal::register::RegisterResult<(), Self::BusError> {
        for (i, byte) in data.iter().enumerate() {
            self.held[address as usize + i] = *byte;
        }
        Ok(())
    }
}

#[test]
fn a_config_write_that_does_not_read_back_is_retried_not_accepted() {
    let mut bus = Stuck {
        stuck: u16::from(ACC_CONF),
        stuck_value: 0x00,
        held: [0; 256],
    };
    // The chip still reads 0x00 for the rate: the write is not taken yet.
    let refused = chip::write_checked(&mut bus, u16::from(ACC_CONF), 0xa9, 0x0f);
    assert!(matches!(
        refused,
        Err(chip::Failure::Register(
            lemnos_hal::register::RegisterError::Mismatch { .. }
        ))
    ));
    assert_eq!(refused.unwrap_err().status(), Status::IO);

    // Once the chip reads back what was written, the same write is accepted.
    // The bits outside the mask (bits 7-4) are not compared.
    bus.stuck = 0x1ff;
    assert_eq!(
        chip::write_checked(&mut bus, u16::from(ACC_CONF), 0xa9, 0x0f),
        Ok(())
    );
}

/// Half the accelerometer FIFO's depth at 200 Hz: the period of its tick drains.
const TICK_NS: u64 = 365_000_000;
/// The accelerometer's 6 g scale, m/s² per count.
const ACC6: f32 = 6.0 * G / 32_768.0;

/// Both dies with their FIFOs on, the accelerometer at `accel_rate`.
fn fifo_device_at(accel_rate: i64) -> (SimBoard, SimI2c, Bmi088) {
    let (mut board, sim) = chip();
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
    let dev = bind(
        &mut board,
        &[
            ("fifo", ConfigValue::Bool(true)),
            ("accel_rate", ConfigValue::Int(accel_rate)),
        ],
    )
    .unwrap();
    (board, sim, dev)
}

/// Four accelerometer reads: the four power-up steps, each `AGAIN`.
fn powered(dev: &mut Bmi088) {
    for step in 0..10_000u64 {
        Driver::tick(dev, step * 1_000_000);
        if Driver::health(dev) == Health::Ok {
            return;
        }
    }
    panic!("the device never came up");
}

/// A clean 200 Hz chip, converged: twenty batches, each one read.
fn converged_clean() -> Harness {
    let mut h = Harness::new(
        Oscillator {
            eps: 0.0,
            drift: 0.0,
        },
        200,
    );
    for _ in 0..20 {
        h.step(Plan::clean());
    }
    h
}

/// Pushes accelerometer frames `from..from + count`, with X the frame's number.
fn push_numbered(sim: &SimI2c, from: i16, count: i16) {
    let mut bytes = Vec::new();
    for n in from..from + count {
        bytes.extend(frame(n, 0, 0));
    }
    sim.fifo_push(ACCEL, &bytes);
}

#[test]
fn a_record_is_the_contract_size_and_the_driver_agrees() {
    assert_eq!(RECORD_LEN, ACCEL_RECORD);
    assert_eq!(RECORD_LEN, GYRO_RECORD);
    let mut buf = [0u8; 64];
    let mut out = Writer::new(&mut buf);
    AccelSample {
        at_ns: 1,
        value: Vector3 {
            x: 1.0,
            y: 2.0,
            z: 3.0,
        },
    }
    .encode(&mut out)
    .unwrap();
    assert_eq!(out.len(), ACCEL_RECORD);
}

#[test]
fn read_batch_needs_the_fifo_and_waits_for_power_up() {
    let (mut board, _sim) = chip();
    let mut dev = bind(&mut board, &[]).unwrap();
    let mut buf = [0u8; ACCEL_RECORD];
    assert_eq!(
        Accelerometer::read_batch(&mut dev, &mut buf),
        Err(Status::NOT_SUPPORTED)
    );

    let (_board, _sim, mut dev) = fifo_device_at(200);
    assert_eq!(
        Accelerometer::read_batch(&mut dev, &mut buf),
        Err(Status::AGAIN)
    );
}

#[test]
fn batches_deliver_every_sample_once_in_order_with_its_stamp() {
    // A clean 200 Hz chip. The clock publishes once it has converged, which is batch 15 (sixteen
    // host-timed batches); until then the batches hold their samples, the ring keeps the newest 146
    // of them, and the older ones are dropped with no loss reported. From the first delivery on,
    // every sample is delivered once, in order, with its stamp within a millisecond of its true
    // time, and the newest is the last one delivered.
    let mut h = Harness::new(
        Oscillator {
            eps: 0.0,
            drift: 0.0,
        },
        200,
    );
    let mut delivered = Vec::new();
    let mut first = None;
    let mut newest_then = 0;
    for batch in 0..40 {
        let got = h.step(Plan::clean());
        if !got.is_empty() && first.is_none() {
            first = Some(batch);
            newest_then = h.seq;
        }
        delivered.extend(got);
        assert_ne!(
            Driver::health(&h.dev),
            Health::Degraded("accelerometer: batch missed samples"),
            "batch {batch}: the convergence's dropped samples are not a batch miss"
        );
    }
    let first = first.expect("the clock never converged");
    assert_eq!(
        delivered[0].seq,
        newest_then - ACC_RING as u64,
        "the first records are the ring's worth before the convergence"
    );
    assert!(first <= 20, "the first delivery is batch {first}");
    for pair in delivered.windows(2) {
        assert_eq!(pair[1].seq, pair[0].seq + 1, "in order, once each");
    }
    assert_eq!(
        delivered.last().unwrap().seq,
        h.seq - 1,
        "the newest is last"
    );
    for d in &delivered {
        let err = d.at_ns as f64 - h.truth(d.seq);
        assert!(
            err.abs() <= 1_000_000.0,
            "sample {} was {err} ns off",
            d.seq
        );
    }
    assert_eq!(Driver::health(&h.dev), Health::Ok);
}

#[test]
fn ticks_alone_keep_the_fifo_from_overflowing() {
    let (board, sim, mut dev) = fifo_device_at(200);
    powered(&mut dev);
    sim.fifo_stream(ACCEL, Duration::from_millis(5), &frame(1, 2, 3));
    let clock = board.clock();
    // Twenty seconds at 200 Hz with nobody reading: ticks alone.
    for _ in 0..4000 {
        clock.advance(Duration::from_millis(5));
        Driver::tick(&mut dev, clock.now_ns());
    }
    assert_eq!(sim.fifo_overflowed(ACCEL), 0, "frames were dropped");
    assert_ne!(
        Driver::health(&dev),
        Health::Degraded("accelerometer: FIFO overflowed, samples lost")
    );
    assert!(Accelerometer::read(&mut dev).is_ok());
}

#[test]
fn a_tick_drains_only_when_due_and_says_when_next() {
    let (_board, sim, mut dev) = fifo_device_at(200);
    assert!(
        dev.accel.tick(5, &mut dev.bus, &*dev.clock).is_some(),
        "not powered: its power-up is due"
    );
    // Power the accelerometer up with its own ticks. The tick that finds it ready drains the
    // (empty) FIFO and sets the first drain a half FIFO later.
    let mut t0 = 5;
    while dev.accel.phase != Phase::Ready {
        t0 += 1_000_000;
        dev.accel.tick(t0, &mut dev.bus, &*dev.clock);
    }
    // The first drains wait a little more than a half FIFO, by less than one sample period, so
    // they fall at every point of the sample grid (`Die::spread`).
    let due = dev.accel.due.expect("the ready tick set the first drain");
    assert!(
        (t0 + TICK_NS..t0 + TICK_NS + 5_000_000).contains(&due),
        "the first drain is a half FIFO and less than a period later: {due}"
    );

    // Counts are relative: the chip's FIFO may already hold frames the stream put there.
    let base = sim.fifo_len(ACCEL);
    push_numbered(&sim, 0, 3);
    assert_eq!(
        dev.accel.tick(t0 + 1_000, &mut dev.bus, &*dev.clock),
        Some(due)
    );
    assert_eq!(
        sim.fifo_len(ACCEL),
        base + 21,
        "not due: nothing drained (7 bytes a frame)"
    );
    push_numbered(&sim, 3, 3);
    assert_eq!(
        dev.accel.tick(due - 1, &mut dev.bus, &*dev.clock),
        Some(due),
        "early tick"
    );
    assert_eq!(
        sim.fifo_len(ACCEL),
        base + 42,
        "an early tick takes nothing"
    );
    let next = dev.accel.tick(due, &mut dev.bus, &*dev.clock).unwrap();
    assert!(
        (due + TICK_NS..due + TICK_NS + 5_000_000).contains(&next),
        "the next drain is a half FIFO and less than a period later: {next}"
    );
    assert_eq!(sim.fifo_len(ACCEL), 0, "due: the FIFO is drained");
}

#[test]
fn a_read_after_a_tick_returns_the_newest_and_stamps_the_rest_on_the_grid() {
    // Once converged, a tick drains a batch. A read returns its newest sample, again until one more
    // arrives, and the batch returns the samples with stamps one sample period apart.
    let mut h = converged_clean();
    let from = h.seq;
    h.fill_and_tick(Plan::clean());
    let (value, seq) = h.read_newest();
    assert_eq!(seq, h.seq - 1, "the read returns the newest sample");
    close(value as f32, seq as f32 * ACC6, 1e-3);
    assert_eq!(h.read_newest().1, seq, "no new sample: the same one again");
    let batch = h.take(160);
    assert_eq!(batch.len() as u64, h.seq - from, "the whole drain");
    for pair in batch.windows(2) {
        let period = pair[1].at_ns - pair[0].at_ns;
        // The sensor's tick is 39 us, so a period is only as exact as its quantisation: 50 ns here.
        assert!(
            period.abs_diff(5_000_000) <= 50,
            "{period} ns between samples"
        );
    }
    for d in &batch {
        assert!((d.at_ns as f64 - h.truth(d.seq)).abs() <= 1_000_000.0);
    }
}

#[test]
fn a_batch_takes_what_fits_and_keeps_the_rest() {
    // A batch takes what its buffer holds and keeps the rest for the next call.
    let mut h = converged_clean();
    let from = h.seq;
    h.fill_and_tick(Plan::clean());
    let mut seen = Vec::new();
    for _ in 0..40 {
        let got = h.take(2);
        assert!(got.len() <= 2, "a batch of two took {}", got.len());
        seen.extend(got);
    }
    assert_eq!(seen.len() as u64, h.seq - from);
    for pair in seen.windows(2) {
        assert_eq!(pair[1].seq, pair[0].seq + 1, "the rest is kept, in order");
    }
    assert!(h.take(2).is_empty());
}

#[test]
fn a_batch_the_ring_overwrote_is_reported_and_the_newest_are_kept() {
    // Three batches with no batch read: the ring (146 frames) keeps only the newest 146, the
    // overwritten ones are reported by the read that finds them, and the next read has nothing new.
    let mut h = converged_clean();
    for _ in 0..3 {
        h.fill_and_tick(Plan::clean());
    }
    let batch = h.take(146);
    assert_eq!(batch.len(), 146);
    assert_eq!(batch.last().unwrap().seq, h.seq - 1, "the newest are kept");
    assert_eq!(
        Driver::health(&h.dev),
        Health::Degraded("accelerometer: batch missed samples")
    );
    assert!(h.take(146).is_empty());
    assert_ne!(
        Driver::health(&h.dev),
        Health::Degraded("accelerometer: batch missed samples")
    );
}

#[test]
fn an_overflow_found_by_a_tick_is_reported_until_the_read_after_it() {
    let (_board, sim, mut dev) = fifo_device_at(200);
    powered(&mut dev);
    // A skip frame: four frames dropped, then one kept.
    let mut bytes = std::vec![0x40, 4];
    bytes.extend(frame(9, 9, 9));
    sim.fifo_push(ACCEL, &bytes);
    Driver::tick(&mut dev, 1_000_000_000);
    let overflow = Health::Degraded("accelerometer: FIFO overflowed, samples lost");
    assert_eq!(Driver::health(&dev), overflow);
    assert!(Accelerometer::read(&mut dev).is_ok());
    assert_eq!(Driver::health(&dev), overflow, "shown by the read");
    assert!(Accelerometer::read(&mut dev).is_ok());
    assert_ne!(Driver::health(&dev), overflow);
}

#[test]
fn the_batch_method_writes_records_into_the_callers_reply() {
    // The batch method writes whole records into the caller's reply, oldest first, and the rest
    // stays for the next call.
    let mut h = converged_clean();
    let oldest = h.seq;
    h.fill_and_tick(Plan::clean());
    let mut buf = [0u8; 64];
    let args = 64u32.to_le_bytes();
    let len = {
        let mut reply = Writer::new(&mut buf);
        let method = MethodId {
            contract: 0,
            method: 2,
        };
        assert_eq!(
            Driver::dispatch(&mut h.dev, method, &args, &mut reply),
            Status::OK
        );
        reply.len()
    };
    assert_eq!(len, 2 * ACCEL_RECORD);
    let first = AccelSample::decode(&mut Reader::new(&buf[..ACCEL_RECORD])).unwrap();
    let second = AccelSample::decode(&mut Reader::new(&buf[ACCEL_RECORD..len])).unwrap();
    close(first.value.x, oldest as f32 * ACC6, 1e-3);
    close(second.value.x, (oldest + 1) as f32 * ACC6, 1e-3);
    assert!(second.at_ns.abs_diff(first.at_ns + 5_000_000) <= 50);
    assert!((first.at_ns as f64 - h.truth(oldest)).abs() <= 1_000_000.0);
}

#[test]
fn a_fifo_length_above_the_fifo_is_clamped_to_it() {
    let (mut board, sim) = chip();
    sim.add_fifo(
        ACCEL,
        ACC_FIFO_DATA,
        FifoLevel::Bytes(ACC_FIFO_LENGTH),
        4096,
    );
    let mut dev = bind(&mut board, &[("fifo", ConfigValue::Bool(true))]).unwrap();
    powered(&mut dev);
    // 157 frames: more than the 1024-byte FIFO can report.
    let mut bytes = Vec::new();
    for n in 0..157i16 {
        bytes.extend(frame(n, 0, 0));
    }
    sim.fifo_push(ACCEL, &bytes);
    assert!(Accelerometer::read(&mut dev).is_ok());
    assert_eq!(
        sim.fifo_len(ACCEL),
        bytes.len() - 1024,
        "at most 1024 bytes read"
    );
}

#[test]
fn gyro_batches_are_stamped_at_the_gyro_rate() {
    let (_board, sim, mut dev) = fifo_chip();
    assert_eq!(Gyroscope::read(&mut dev), Err(Status::AGAIN));
    powered(&mut dev);
    let mut bytes = Vec::new();
    for n in 1..=10i16 {
        bytes.extend(gyro_frame(n, 0, 0));
    }
    sim.fifo_push(GYRO, &bytes);
    let now = 3_000_000_000;
    Driver::tick(&mut dev, now);
    let mut buf = [0u8; GYRO_RECORD * 16];
    let n = Gyroscope::read_batch(&mut dev, &mut buf).unwrap();
    assert_eq!(n, 10 * GYRO_RECORD);
    for (i, record) in buf[..n].as_chunks::<GYRO_RECORD>().0.iter().enumerate() {
        let sample = GyroSample::decode(&mut Reader::new(record)).unwrap();
        // 2000 Hz: 0.5 ms apart, the newest at the tick.
        assert_eq!(sample.at_ns, now - (9 - i as u64) * 500_000);
        close(
            sample.value.x,
            (i as f32 + 1.0) * 2000.0 / 32_768.0 * DPS,
            1e-4,
        );
    }
}

/// A die 1 % fast of its 5 ms nominal period, drained every 365 ms (with up to
/// 0.1 ms of jitter each time). Each batch's stamps are checked against the
/// true times of its samples: the newest is within one period of the tick, and
/// once the period is measured, the older ones stay within a period plus their
/// age times the measurement's error.
#[test]
fn stamps_follow_a_die_1_5_percent_fast_within_a_millisecond() {
    // A die 1.5 % fast takes its samples every 4.926 ms of host time, not 5. The stamps follow it
    // to within a millisecond, under host jitter of a few hundred microseconds.
    let mut h = Harness::new(
        Oscillator {
            eps: 0.015,
            drift: 0.0,
        },
        200,
    );
    let mut jitter = Jitter::new(0x1357_9bdf);
    let mut delivered = Vec::new();
    for _ in 0..60 {
        let host = jitter.next(50_000);
        delivered.extend(h.step(Plan {
            tick_jitter_ns: jitter.next(300_000),
            pre_ns: host - 100_000,
            post_ns: host + 100_000,
            report_early_ns: 0,
            fault: None,
        }));
    }
    assert!(delivered.len() > 2_000, "only {} samples", delivered.len());
    let first = delivered.first().unwrap();
    let last = delivered.last().unwrap();
    let mean = (last.at_ns - first.at_ns) as f64 / (last.seq - first.seq) as f64;
    let true_mean = 5_000_000.0 / 1.015;
    assert!(
        (mean - true_mean).abs() <= 1_000.0,
        "a period of {mean} ns, not {true_mean}"
    );
    for d in &delivered {
        let err = d.at_ns as f64 - h.truth(d.seq);
        assert!(
            err.abs() <= 1_000_000.0,
            "sample {} was {err} ns off",
            d.seq
        );
    }
}

#[test]
fn a_full_valid_config_passes_the_check() {
    let entries = [
        ("accel_address", ConfigValue::Int(0x19)),
        ("accel_range", ConfigValue::Int(12)),
        ("accel_rate", ConfigValue::Float(12.5)),
        ("gyro_address", ConfigValue::Int(0x69)),
        ("gyro_range", ConfigValue::Int(500)),
        ("gyro_rate", ConfigValue::Int(400)),
        ("fifo", ConfigValue::Bool(true)),
    ];
    assert_eq!(
        <Bmi088 as Driver>::check_config(&Config::new(&entries)),
        Ok(())
    );
}

#[test]
fn an_unknown_key_is_refused_and_a_near_miss_is_suggested() {
    let bogus = [("bogus", ConfigValue::Int(1))];
    let error = <Bmi088 as Driver>::check_config(&Config::new(&bogus)).unwrap_err();
    assert_eq!(error.key, "bogus");
    let near = [("accel_rang", ConfigValue::Int(6))];
    let error = <Bmi088 as Driver>::check_config(&Config::new(&near)).unwrap_err();
    assert_eq!(error.suggestion, Some("accel_range"));
}

#[test]
fn a_wrong_kind_is_refused_by_name() {
    let entries = [("fifo", ConfigValue::Int(1))];
    let error = <Bmi088 as Driver>::check_config(&Config::new(&entries)).unwrap_err();
    assert_eq!(error.key, "fifo");
}

#[test]
fn a_range_or_rate_the_chip_cannot_set_is_refused_by_its_key() {
    for (key, value, expect) in [
        ("accel_range", ConfigValue::Int(5), "accel_range"),
        ("accel_rate", ConfigValue::Float(13.0), "accel_rate"),
        ("gyro_range", ConfigValue::Int(4000), "gyro_range"),
        ("gyro_rate", ConfigValue::Int(150), "gyro_rate"),
    ] {
        let entries = [(key, value)];
        let error = <Bmi088 as Driver>::check_config(&Config::new(&entries)).unwrap_err();
        assert_eq!(error.key, expect, "{key}");
    }
    // The edges the chip does set are accepted.
    let edges = [
        ("accel_range", ConfigValue::Int(24)),
        ("accel_rate", ConfigValue::Float(1600.0)),
        ("gyro_range", ConfigValue::Int(125)),
        ("gyro_rate", ConfigValue::Int(100)),
    ];
    assert_eq!(
        <Bmi088 as Driver>::check_config(&Config::new(&edges)),
        Ok(())
    );
}

#[test]
fn an_address_outside_the_usable_seven_bit_range_is_refused() {
    for (value, ok) in [(0x07, false), (0x08, true), (0x77, true), (0x78, false)] {
        let entries = [("accel_address", ConfigValue::Int(value))];
        assert_eq!(
            <Bmi088 as Driver>::check_config(&Config::new(&entries)).is_ok(),
            ok,
            "{value:#x}"
        );
    }
}

/// The item as the wire carries it: what a client renders a reply with.
fn wire_item(item: &lemnos_core::Item) -> lemnos_wire::OwnedItem {
    lemnos_wire::OwnedItem {
        kind: lemnos_core::ItemKind::Value,
        name: item.name.to_string(),
        unit: item.unit.symbol().to_string(),
        unit_state: item.unit_state,
        exponent: item.exponent,
        min: item.min,
        max: item.max,
        real_min: item.real_min,
        real_max: item.real_max,
        real_step: item.real_step,
        real_warn_low: item.real_warn_low,
        real_warn_high: item.real_warn_high,
        contract: item.method.contract,
        method: item.method.method,
        field: item.field.map(str::to_string),
        reading: None,
        step: item.step,
        warn_low: item.warn_low,
        warn_high: item.warn_high,
        group: item.group.map(str::to_string),
    }
}

/// One g reads as about 9.81 m/s² through the display path: the reply rendered with the item's
/// exponent, as a client shows it. Fails before the fix: the exponent was -3 and 1 g showed as
/// 0.00981 m/s².
#[test]
fn one_g_displays_as_about_nine_point_eight_one_metres_per_second_squared() {
    let (mut board, sim) = chip();
    let mut dev = bind(&mut board, &[]).unwrap();
    accel_up(&mut dev);
    // 6 g full scale: 5461 counts is one g.
    sim.set_bytes(ACCEL, ACC_DATA, &axes_bytes(5461, 0, 0));
    let sample = Accelerometer::read(&mut dev).unwrap();
    // The reply as the dispatch writes it: a record block of its three f32 axes.
    let mut reply = Vec::new();
    reply.extend_from_slice(&12u32.to_le_bytes());
    reply.extend_from_slice(&sample.x.to_le_bytes());
    reply.extend_from_slice(&sample.y.to_le_bytes());
    reply.extend_from_slice(&sample.z.to_le_bytes());
    let shape = lemnos_wire::OwnedShape::Record(std::vec![
        ("x".to_string(), lemnos_wire::OwnedShape::F32),
        ("y".to_string(), lemnos_wire::OwnedShape::F32),
        ("z".to_string(), lemnos_wire::OwnedShape::F32),
    ]);
    let mut out = Description::new();
    Driver::describe(&dev, &mut out);
    let item = out.find("acceleration.x").unwrap();
    let shown = lemnos_wire::text::render(&shape, false, &reply, &wire_item(item)).unwrap();
    let number: f64 = shown.split(' ').next().unwrap().parse().unwrap();
    assert!((number - 9.81).abs() < 0.01, "shown as {shown}");
    assert!(shown.ends_with("m/s²"), "shown as {shown}");
    assert_eq!(item.exponent, 0, "a real value is in base units");
}
