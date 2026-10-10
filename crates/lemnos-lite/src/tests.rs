extern crate std;

use super::*;
use lemnos_drivers_bmi088::{ACCEL_ADDRESS, ACCEL_CHIP_ID, Bmi088, GYRO_ADDRESS, GYRO_CHIP_ID};
use lemnos_drivers_ina2xx::{Config, DEFAULT_ADDRESS, Ina, Model};
use lemnos_drivers_vcm::Vcm;
use lemnos_hal::AddressWidth;
use lemnos_hal::mock::{MockDelay, MockI2c};
use std::vec::Vec;

fn imu_bus() -> MockI2c {
    MockI2c::new()
        .with_target(ACCEL_ADDRESS, AddressWidth::Bits8)
        .with_target(GYRO_ADDRESS, AddressWidth::Bits8)
        .with_registers(ACCEL_ADDRESS, 0x00, &[ACCEL_CHIP_ID])
        .with_registers(GYRO_ADDRESS, 0x00, &[GYRO_CHIP_ID])
        .with_registers(ACCEL_ADDRESS, 0x12, &[0x00, 0x40, 0, 0, 0, 0])
}

#[test]
fn table_tracks_status_and_reads_by_name_or_index() {
    let mut imu = Bmi088::new(imu_bus());
    // Nothing answers at 0x40: the power monitor is missing.
    let mut power = Ina::new(
        MockI2c::new(),
        DEFAULT_ADDRESS,
        Model::Ina238,
        Config::from_micro(10_000, 2_000_000),
    )
    .unwrap();
    let mut lens = Vcm::dw9817(MockI2c::new().with_raw_target(0x0c));
    let mut devices = Devices::new([
        sensor("imu", &mut imu),
        sensor("power", &mut power),
        control("lens", &mut lens),
    ]);
    let mut buf = [0; MAX_CHANNELS];
    assert_eq!(devices.status("imu"), DeviceStatus::Missing);
    assert_eq!(devices.read("imu", &mut buf), Err(ErrorKind::Unavailable));

    assert_eq!(devices.init_all(&mut MockDelay::new()), 2);
    assert_eq!(devices.status("imu"), DeviceStatus::Available);
    assert_eq!(devices.status(1), DeviceStatus::Missing);
    assert!(devices.last_error("power").is_some());
    assert_eq!(devices.status("nope"), DeviceStatus::Missing);

    devices.read("imu", &mut buf).unwrap();
    // Half of ±6 g: 29.42 m/s².
    assert_eq!(buf[0], 29_419);
    assert_eq!(devices.read(1, &mut buf), Err(ErrorKind::Unavailable));
    assert_eq!(devices.read("lens", &mut buf), Err(ErrorKind::Unsupported));
    // An unsupported operation marks the device faulted until it works again.
    assert_eq!(devices.status("lens"), DeviceStatus::Faulted);
    assert_eq!(devices.set_named("lens", "position", 300), Ok(300));
    assert_eq!(devices.status("lens"), DeviceStatus::Available);
    assert_eq!(devices.get(2, 0), Ok(300));
    assert_eq!(devices.set(2, 0, 5000), Err(ErrorKind::InvalidInput));
    assert_eq!(devices.info("power").unwrap().model, "INA238");
    assert_eq!(devices.name(2), Some("lens"));
    assert_eq!(devices.len(), 3);
}

#[test]
fn poll_reads_due_sensors_and_retries_missing_ones() {
    let mut imu = Bmi088::new(imu_bus());
    let mut power = Ina::new(
        MockI2c::new(),
        DEFAULT_ADDRESS,
        Model::Ina226,
        Config::from_micro(10_000, 2_000_000),
    )
    .unwrap();
    let mut devices = Devices::new([
        sensor("imu", &mut imu).every(10),
        sensor("power", &mut power).every(100),
    ]);
    let mut delay = MockDelay::new();
    let mut buf = [0; MAX_CHANNELS];
    let mut seen = Vec::new();
    for now in [0, 5, 10, 20, 100] {
        devices.poll(now, &mut delay, &mut buf, |reading| {
            seen.push((now, reading.name, reading.values.len(), reading.values[0]));
        });
    }
    // The IMU is brought up on its first poll and read every 10 ms; the
    // power monitor never answers, so it is retried on its own period.
    assert_eq!(
        seen,
        [
            (0, "imu", 12, 29_419),
            (10, "imu", 12, 29_419),
            (20, "imu", 12, 29_419),
            (100, "imu", 12, 29_419)
        ]
    );
    assert_eq!(devices.status("power"), DeviceStatus::Missing);
    assert_eq!(devices.last_error("power"), Some(ErrorKind::Nack));
}
