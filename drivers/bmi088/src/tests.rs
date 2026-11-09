//! The driver on a simulated bus: each die's registers are the sim's bytes, and
//! each die's FIFO is the sim's FIFO.

extern crate std;

use super::*;
use chip::{Frame, scan_accel};
use lemnos_contract_accelerometer::{RECORD_LEN as ACCEL_RECORD, Sample as AccelSample};
use lemnos_contract_gyroscope::{RECORD_LEN as GYRO_RECORD, Sample as GyroSample};
use lemnos_core::{Decode, Description, Driver, Reader, Resources, Writer};
use lemnos_sim::{FifoLevel, Layout, SimBoard, SimI2c};
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

/// Reads the accelerometer until it has a sample, and returns it.
fn accel_up(dev: &mut Bmi088) -> Vector3 {
    for _ in 0..10 {
        match Accelerometer::read(dev) {
            Ok(sample) => return sample,
            Err(status) => assert_eq!(status, Status::AGAIN),
        }
    }
    panic!("accelerometer never became ready");
}

/// Reads the gyroscope until it has a sample, and returns it.
fn gyro_up(dev: &mut Bmi088) -> Vector3 {
    for _ in 0..10 {
        match Gyroscope::read(dev) {
            Ok(sample) => return sample,
            Err(status) => assert_eq!(status, Status::AGAIN),
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
fn accelerometer_powers_up_one_step_per_read() {
    let (mut board, sim) = chip();
    sim.set_bytes(ACCEL, ACC_DATA, &axes_bytes(16384, 0, 0));
    let mut dev = bind(&mut board, &[]).unwrap();
    assert_eq!(
        Driver::health(&dev),
        Health::Degraded("accelerometer: not started")
    );

    assert_eq!(Accelerometer::read(&mut dev), Err(Status::AGAIN));
    assert_eq!(sim.byte(ACCEL, ACC_SOFTRESET), Some(0xb6));
    assert_eq!(
        Driver::health(&dev),
        Health::Degraded("accelerometer: powering up")
    );

    assert_eq!(Accelerometer::read(&mut dev), Err(Status::AGAIN));
    assert_eq!(sim.byte(ACCEL, ACC_PWR_CONF), Some(0x00));
    assert_eq!(sim.byte(ACCEL, ACC_RANGE), None);

    assert_eq!(Accelerometer::read(&mut dev), Err(Status::AGAIN));
    assert_eq!(sim.byte(ACCEL, ACC_PWR_CTRL), Some(0x04));
    assert_eq!(sim.byte(ACCEL, ACC_RANGE), None);

    // The configuration step writes the settings and reads the sample.
    let sample = Accelerometer::read(&mut dev).unwrap();
    assert_eq!(sim.byte(ACCEL, ACC_RANGE), Some(0x01));
    assert_eq!(sim.byte(ACCEL, ACC_CONF), Some(0xa8));
    close(sample.x, 3.0 * G, 1e-3);
    // The gyroscope has not been read, so the device is still degraded.
    assert_eq!(
        Driver::health(&dev),
        Health::Degraded("gyroscope: not started")
    );
}

#[test]
fn gyroscope_powers_up_in_two_steps_and_reads_in_rad_per_s() {
    let (mut board, sim) = chip();
    sim.set_bytes(GYRO, GYR_DATA, &axes_bytes(16384, -16384, 0));
    let mut dev = bind(&mut board, &[]).unwrap();

    assert_eq!(Gyroscope::read(&mut dev), Err(Status::AGAIN));
    assert_eq!(sim.byte(GYRO, GYR_SOFTRESET), Some(0xb6));
    assert_eq!(Gyroscope::read(&mut dev), Err(Status::AGAIN));
    assert_eq!(sim.byte(GYRO, GYR_RANGE), None);

    let sample = Gyroscope::read(&mut dev).unwrap();
    // Default 2000 degrees per second: half scale is 1000.
    assert_eq!(sim.byte(GYRO, GYR_RANGE), Some(0x00));
    assert_eq!(sim.byte(GYRO, GYR_BANDWIDTH), Some(0x00));
    close(sample.x, 1000.0 * DPS, 1e-4);
    close(sample.y, -1000.0 * DPS, 1e-4);
    close(sample.z, 0.0, 1e-6);
}

#[test]
fn wrong_chip_is_refused_and_nothing_is_written() {
    let (mut board, sim) = chip();
    sim.set_bytes(ACCEL, ACC_ID, &[0x00]);
    let mut dev = bind(&mut board, &[]).unwrap();

    assert_eq!(Accelerometer::read(&mut dev), Err(Status::NOT_SUPPORTED));
    assert_eq!(sim.byte(ACCEL, ACC_SOFTRESET), None);
    assert_eq!(
        Driver::health(&dev),
        Health::Degraded("accelerometer: wrong chip id")
    );

    // Once the right chip is there, the next read powers it up.
    sim.set_bytes(ACCEL, ACC_ID, &[0x1e]);
    assert_eq!(Accelerometer::read(&mut dev), Err(Status::AGAIN));
    assert_eq!(sim.byte(ACCEL, ACC_SOFTRESET), Some(0xb6));
}

#[test]
fn a_missing_gyroscope_does_not_stop_the_accelerometer() {
    let (mut board, sim) = chip();
    sim.set_dead(GYRO, true);
    let mut dev = bind(&mut board, &[]).unwrap();
    accel_up(&mut dev);

    // The transfer fails on that read, and the gyroscope is retried on the next.
    assert_eq!(Gyroscope::read(&mut dev), Err(Status::IO));
    assert_eq!(
        Driver::health(&dev),
        Health::Degraded("gyroscope: transfer failed")
    );
    assert!(Accelerometer::read(&mut dev).is_ok());

    sim.set_dead(GYRO, false);
    assert_eq!(Gyroscope::read(&mut dev), Err(Status::AGAIN));
    assert_eq!(sim.byte(GYRO, GYR_SOFTRESET), Some(0xb6));
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
        Health::Degraded("accelerometer: transfer failed")
    );

    // The chip comes back: the next read soft-resets it and starts over.
    sim.set_bytes(ACCEL, ACC_SOFTRESET, &[0]);
    sim.set_dead(ACCEL, false);
    assert_eq!(Accelerometer::read(&mut dev), Err(Status::AGAIN));
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
    assert_eq!(Accelerometer::read(&mut dev), Err(Status::AGAIN));
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
    for _ in 0..4 {
        assert_eq!(Accelerometer::read(dev), Err(Status::AGAIN));
    }
}

/// Pushes accelerometer frames `from..from + count`, with X the frame's number.
fn push_numbered(sim: &SimI2c, from: i16, count: i16) {
    let mut bytes = Vec::new();
    for n in from..from + count {
        bytes.extend(frame(n, 0, 0));
    }
    sim.fifo_push(ACCEL, &bytes);
}

/// Takes one batch of accelerometer records, decoded.
fn accel_batch(dev: &mut Bmi088, buf: &mut [u8]) -> Vec<AccelSample> {
    let n = Accelerometer::read_batch(dev, buf).unwrap();
    assert_eq!(n % ACCEL_RECORD, 0, "whole records only");
    buf[..n]
        .as_chunks::<ACCEL_RECORD>()
        .0
        .iter()
        .map(|record| AccelSample::decode(&mut Reader::new(record)).unwrap())
        .collect()
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
    let (_board, sim, mut dev) = fifo_device_at(200);
    powered(&mut dev);
    let mut buf = [0u8; ACCEL_RECORD * 146];
    let mut next = 0i16;
    let mut delivered = 0;
    for drain in 1..=6u64 {
        let now = 1_000_000_000 + drain * TICK_NS;
        push_numbered(&sim, next, 73);
        Driver::tick(&mut dev, now);
        let batch = accel_batch(&mut dev, &mut buf);
        assert_eq!(batch.len(), 73, "drain {drain}");
        for (i, sample) in batch.iter().enumerate() {
            // The newest is stamped at the tick; each older one a period back.
            let back = (72 - i) as u64;
            assert_eq!(sample.at_ns, now - back * 5_000_000, "drain {drain}");
            close(sample.value.x, f32::from(next + i as i16) * ACC6, 1e-3);
        }
        next += 73;
        delivered += batch.len();
    }
    assert_eq!(delivered, 6 * 73);
    assert_eq!(sim.fifo_len(ACCEL), 0);
    assert_eq!(
        Driver::health(&dev),
        Health::Degraded("gyroscope: not started")
    );
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
    assert_eq!(Driver::tick(&mut dev, 5), None, "not powered: nothing due");
    powered(&mut dev);
    push_numbered(&sim, 0, 3);
    assert_eq!(Driver::tick(&mut dev, 1_000), Some(1_000 + TICK_NS));
    assert_eq!(sim.fifo_len(ACCEL), 0);
    push_numbered(&sim, 3, 3);
    assert_eq!(
        Driver::tick(&mut dev, 1_000 + TICK_NS - 1),
        Some(1_000 + TICK_NS),
        "early tick"
    );
    assert_eq!(sim.fifo_len(ACCEL), 21, "an early tick takes nothing");
    assert_eq!(
        Driver::tick(&mut dev, 1_000 + TICK_NS),
        Some(1_000 + 2 * TICK_NS)
    );
    assert_eq!(sim.fifo_len(ACCEL), 0);
}

#[test]
fn a_read_after_a_tick_returns_the_newest_and_stamps_the_rest_on_the_grid() {
    let (_board, sim, mut dev) = fifo_device_at(200);
    powered(&mut dev);
    let t = 1_000_000_000;
    push_numbered(&sim, 1, 1);
    Driver::tick(&mut dev, t);
    // Two more samples arrive after the tick, and a read takes them.
    push_numbered(&sim, 2, 2);
    let newest = Accelerometer::read(&mut dev).unwrap();
    close(newest.x, 3.0 * ACC6, 1e-3);
    assert_eq!(Accelerometer::read(&mut dev), Ok(newest), "no new sample");

    let mut buf = [0u8; ACCEL_RECORD * 8];
    let batch = accel_batch(&mut dev, &mut buf);
    assert_eq!(
        batch.iter().map(|s| s.at_ns).collect::<Vec<_>>(),
        [t, t + 5_000_000, t + 10_000_000]
    );
    close(batch[0].value.x, ACC6, 1e-3);
    close(batch[2].value.x, 3.0 * ACC6, 1e-3);
}

#[test]
fn a_batch_takes_what_fits_and_keeps_the_rest() {
    let (_board, sim, mut dev) = fifo_device_at(200);
    powered(&mut dev);
    push_numbered(&sim, 0, 5);
    Driver::tick(&mut dev, 1_000_000_000);
    let mut buf = [0u8; ACCEL_RECORD * 2];
    let mut seen = Vec::new();
    for _ in 0..3 {
        seen.extend(accel_batch(&mut dev, &mut buf));
    }
    assert_eq!(seen.len(), 5);
    for (i, sample) in seen.iter().enumerate() {
        close(sample.value.x, i as f32 * ACC6, 1e-3);
    }
    assert!(accel_batch(&mut dev, &mut buf).is_empty());
}

#[test]
fn a_batch_the_ring_overwrote_is_reported_and_the_newest_are_kept() {
    let (_board, sim, mut dev) = fifo_device_at(200);
    powered(&mut dev);
    // Two drains of a full FIFO each: the ring (146 frames) keeps only the second.
    push_numbered(&sim, 0, 146);
    Driver::tick(&mut dev, 1_000_000_000);
    push_numbered(&sim, 146, 146);
    Driver::tick(&mut dev, 1_000_000_000 + TICK_NS);
    let mut buf = [0u8; ACCEL_RECORD * 146];
    let batch = accel_batch(&mut dev, &mut buf);
    assert_eq!(batch.len(), 146);
    close(batch[0].value.x, 146.0 * ACC6, 1e-3);
    assert_eq!(
        Driver::health(&dev),
        Health::Degraded("accelerometer: batch missed samples")
    );
    // Reported by that read; the next one has nothing new to report.
    assert!(accel_batch(&mut dev, &mut buf).is_empty());
    assert_ne!(
        Driver::health(&dev),
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
    let (_board, sim, mut dev) = fifo_device_at(200);
    powered(&mut dev);
    push_numbered(&sim, 7, 2);
    let now = 2_000_000_000;
    Driver::tick(&mut dev, now);
    let mut buf = [0u8; 64];
    let args = 64u32.to_le_bytes();
    let len = {
        let mut reply = Writer::new(&mut buf);
        let method = MethodId {
            contract: 0,
            method: 2,
        };
        assert_eq!(
            Driver::dispatch(&mut dev, method, &args, &mut reply),
            Status::OK
        );
        reply.len()
    };
    assert_eq!(len, 2 * ACCEL_RECORD);
    let first = AccelSample::decode(&mut Reader::new(&buf[..ACCEL_RECORD])).unwrap();
    let second = AccelSample::decode(&mut Reader::new(&buf[ACCEL_RECORD..len])).unwrap();
    assert_eq!((first.at_ns, second.at_ns), (now - 5_000_000, now));
    close(first.value.x, 7.0 * ACC6, 1e-3);
    close(second.value.x, 8.0 * ACC6, 1e-3);
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
    for _ in 0..3 {
        assert_eq!(Gyroscope::read(&mut dev), Err(Status::AGAIN));
    }
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
fn stamps_stay_within_a_period_and_the_measured_period_tracks_the_die() {
    const NOMINAL: u64 = 5_000_000;
    const DIE: u64 = 4_950_500;
    let (_board, sim, mut dev) = fifo_device_at(200);
    powered(&mut dev);
    let base = 1_000_000_000u64;
    let mut now = base;
    let mut taken = 0u64;
    let mut buf = [0u8; ACCEL_RECORD * 146];
    let mut first_newest = None;
    let mut worst_old = 0i128;
    for drain in 0..10u64 {
        now += TICK_NS + drain * 10_000;
        // Samples the die has taken by now: sample k at base + k * DIE.
        let produced = (now - base) / DIE + 1;
        let mut bytes = Vec::new();
        for k in taken..produced {
            bytes.extend(frame(k as i16, 0, 0));
        }
        sim.fifo_push(ACCEL, &bytes);
        taken = produced;
        Driver::tick(&mut dev, now);
        let batch = accel_batch(&mut dev, &mut buf);
        let newest_k = taken - 1;
        let newest_true = base + newest_k * DIE;
        let newest_at = batch.last().unwrap().at_ns;
        assert_eq!(newest_at, now);
        assert!(newest_true <= now && now - newest_true < DIE, "one period");
        let first = *first_newest.get_or_insert(newest_k);
        let span = newest_k - first;
        if drain >= 1 {
            // Each stamp's error is the newest's phase (below one period) plus its
            // age times the measured period's error, which is below DIE / span. So
            // the spread across the batch is the ages times that error, and the
            // stamps of a batch are no further than one period from the truth.
            let ages = batch.len() as i128 - 1;
            let spread_bound = ages * DIE as i128 / span as i128 + 2;
            let mut errors = Vec::new();
            for (i, sample) in batch.iter().enumerate() {
                let k = newest_k - (batch.len() - 1 - i) as u64;
                let truth = (base + k * DIE) as i128;
                errors.push(sample.at_ns as i128 - truth);
            }
            let (lo, hi) = (*errors.iter().min().unwrap(), *errors.iter().max().unwrap());
            assert!(
                hi - lo <= spread_bound,
                "drain {drain}: spread {} > {spread_bound}",
                hi - lo
            );
            assert!(
                lo >= -(DIE as i128) && hi <= DIE as i128,
                "drain {drain}: {lo}..{hi}"
            );
            worst_old = worst_old.max(hi - lo);
        }
    }
    assert!(worst_old > 0, "batches were checked");
    let measured = dev.accel.timing.measured.expect("the period is measured");
    let span = (taken - 1) - first_newest.unwrap();
    assert!(
        measured.abs_diff(DIE) * span <= DIE,
        "measured {measured} for a die of {DIE} over {span} samples"
    );
    assert_ne!(measured, NOMINAL);
}
