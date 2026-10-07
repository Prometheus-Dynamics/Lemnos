use super::*;
use lemnos_hal::mock::{MockDelay, MockI2c, block_on};

fn close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() <= expected.abs() * 1e-4 + 1e-6,
        "{actual} != {expected}"
    );
}

fn axes(values: [i16; 3]) -> [u8; 6] {
    let mut bytes = [0u8; 6];
    for (chunk, value) in bytes.chunks_mut(2).zip(values) {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// Both BMI088 dies on one bus.
fn imu() -> MockI2c {
    MockI2c::new()
        .with_target(ACCEL_ADDRESS, AddressWidth::Bits8)
        .with_target(GYRO_ADDRESS, AddressWidth::Bits8)
        .with_registers(ACCEL_ADDRESS, ACC_CHIP_ID, &[ACCEL_CHIP_ID])
        .with_registers(GYRO_ADDRESS, GYR_CHIP_ID, &[GYRO_CHIP_ID])
}

#[test]
fn init_resets_powers_on_and_configures_both_dies() {
    let mut bmi = Bmi088::new(imu());
    let mut delay = MockDelay::new();
    let config = Config {
        accel_range: AccelRange::G12,
        accel_rate: AccelRate::Hz400,
        gyro_range: GyroRange::Dps500,
        gyro_rate: GyroRate::Hz400Bw47,
    };
    bmi.init(&mut delay, config).unwrap();

    assert_eq!(bmi.config(), Some(config));
    assert_eq!(delay.total_ns, 37_000_000);
    let i2c = bmi.release();
    let accel = |register| i2c.register(ACCEL_ADDRESS, register);
    let gyro = |register| i2c.register(GYRO_ADDRESS, register);
    assert_eq!(accel(ACC_SOFTRESET), SOFTRESET);
    assert_eq!(gyro(GYR_SOFTRESET), SOFTRESET);
    assert_eq!(accel(ACC_PWR_CONF), 0x00);
    assert_eq!(accel(ACC_PWR_CTRL), 0x04);
    assert_eq!(accel(ACC_RANGE), 0x02);
    assert_eq!(accel(ACC_CONF), 0xaa);
    assert_eq!(gyro(GYR_RANGE), 0x02);
    assert_eq!(gyro(GYR_BANDWIDTH), 0x03);
}

#[test]
fn reads_si_units_for_the_configured_ranges() {
    let i2c = imu()
        .with_registers(ACCEL_ADDRESS, ACC_DATA, &axes([16384, -16384, 0]))
        .with_registers(GYRO_ADDRESS, GYR_DATA, &axes([16384, 0, -32768]));
    let mut bmi = Bmi088::new(i2c);
    bmi.init(&mut MockDelay::new(), Config::default()).unwrap();

    let sample = bmi.read().unwrap();
    assert_eq!(sample.accel_raw, [16384, -16384, 0]);
    // Half of ±6 g.
    close(sample.accel_mps2[0], 3.0 * STANDARD_GRAVITY);
    close(sample.accel_mps2[1], -3.0 * STANDARD_GRAVITY);
    close(sample.accel_mps2[2], 0.0);
    // Half of ±2000 °/s, and full negative scale.
    close(sample.gyro_radps[0], 1000.0_f32.to_radians());
    close(sample.gyro_radps[2], -2000.0_f32.to_radians());
}

#[test]
fn decodes_temperature() {
    close(decode_temperature(0x00, 0x00), 23.0);
    close(decode_temperature(0x02, 0x00), 25.0);
    // 0x7FF is -1 in 11-bit two's complement.
    close(decode_temperature(0xff, 0xe0), 22.875);
    let i2c = imu().with_registers(ACCEL_ADDRESS, ACC_TEMP, &[0x02, 0x00]);
    close(Bmi088::new(i2c).temperature_c().unwrap(), 25.0);
}

#[test]
fn rejects_wrong_chip_before_writing() {
    let i2c = imu().with_registers(GYRO_ADDRESS, GYR_CHIP_ID, &[0x00]);
    let mut bmi = Bmi088::new(i2c);
    assert_eq!(bmi.read().unwrap_err(), Error::NotInitialized);
    let error = bmi
        .init(&mut MockDelay::new(), Config::default())
        .unwrap_err();
    assert_eq!(
        error,
        Error::WrongChip {
            accel: ACCEL_CHIP_ID,
            gyro: 0
        }
    );
    assert_eq!(error.kind(), ErrorKind::Unsupported);
    assert_eq!(bmi.release().register(ACCEL_ADDRESS, ACC_SOFTRESET), 0);
}

#[test]
fn missing_die_reports_a_bus_error() {
    let i2c = MockI2c::new()
        .with_target(ACCEL_ADDRESS, AddressWidth::Bits8)
        .with_registers(ACCEL_ADDRESS, ACC_CHIP_ID, &[ACCEL_CHIP_ID]);
    let error = Bmi088::new(i2c)
        .init(&mut MockDelay::new(), Config::default())
        .unwrap_err();
    assert!(matches!(error, Error::Register(_)));
    assert_eq!(error.kind(), ErrorKind::Nack);
}

#[test]
fn async_driver_matches_blocking() {
    let i2c = imu().with_registers(ACCEL_ADDRESS, ACC_DATA, &axes([16384, 0, 0]));
    let mut bmi = asynch::Bmi088::new(i2c);
    block_on(bmi.init(&mut MockDelay::new(), Config::default())).unwrap();
    close(
        block_on(bmi.read()).unwrap().accel_mps2[0],
        3.0 * STANDARD_GRAVITY,
    );
}

#[test]
fn resume_reads_without_reinitializing() {
    let i2c = imu().with_registers(GYRO_ADDRESS, GYR_DATA, &axes([0, 16384, 0]));
    let config = Config {
        gyro_range: GyroRange::Dps250,
        ..Config::default()
    };
    let mut bmi = Bmi088::resume(i2c, ACCEL_ADDRESS, GYRO_ADDRESS, config);
    close(bmi.read().unwrap().gyro_radps[1], 125.0_f32.to_radians());
    assert_eq!(bmi.release().register(ACCEL_ADDRESS, ACC_SOFTRESET), 0);
}

#[test]
fn fixed_point_matches_float() {
    let i2c = imu()
        .with_registers(ACCEL_ADDRESS, ACC_DATA, &axes([16384, -16384, 1]))
        .with_registers(GYRO_ADDRESS, GYR_DATA, &axes([16384, -32768, 7]));
    let config = Config {
        accel_range: AccelRange::G24,
        ..Config::default()
    };
    let mut bmi = Bmi088::resume(i2c, ACCEL_ADDRESS, GYRO_ADDRESS, config);
    let fixed = bmi.read_fixed().unwrap();
    assert_eq!(fixed.accel_mg, [12_000, -12_000, 0]);
    // 7 × 2_000_000 / 32768 = 427.2 → 427.
    assert_eq!(fixed.gyro_mdps, [1_000_000, -2_000_000, 427]);
    let float = bmi.read().unwrap();
    for axis in 0..2 {
        close(
            fixed.accel_mg[axis] as f32 / 1000.0 * STANDARD_GRAVITY,
            float.accel_mps2[axis],
        );
    }
    assert_eq!(decode_temperature_mc(0xff, 0xe0), 22_875);
    assert_eq!(bmi.temperature_mc().unwrap(), 23_000);
}

#[test]
fn device_model_reads_mm_per_s2_and_urad_per_s() {
    use lemnos_device::{DeviceRef, NO_VALUE};
    let i2c = imu()
        .with_registers(ACCEL_ADDRESS, ACC_DATA, &axes([16384, -16384, 0]))
        .with_registers(GYRO_ADDRESS, GYR_DATA, &axes([16384, 0, -32768]));
    let config = Config {
        accel_range: AccelRange::G3,
        ..Config::default()
    };
    let mut bmi = Bmi088::new(i2c).with_config(config);
    let mut device = DeviceRef::sensor(&mut bmi);
    let mut out = [NO_VALUE; 7];
    assert_eq!(
        device.read(&mut out),
        Err(lemnos_hal::ErrorKind::Unavailable)
    );
    device.init(&mut MockDelay::new()).unwrap();
    device.read(&mut out).unwrap();
    assert_eq!(device.info().channels.len(), 6);
    // Half of ±3 g is 14.709975 m/s².
    assert_eq!(&out[..3], &[14_709, -14_710, 0]);
    // Half of ±2000 °/s is 17.453293 rad/s.
    assert_eq!(&out[3..6], &[17_453_292, 0, -34_906_586]);
    assert_eq!(out[6], NO_VALUE);
    assert_eq!(bmi.config(), Some(config));
    assert_eq!(KERNEL.channels.len(), INFO.channels.len());
}

#[test]
fn async_device_model_matches_blocking() {
    use lemnos_device::asynch::{Device, Sensor};
    let i2c = imu().with_registers(ACCEL_ADDRESS, ACC_DATA, &axes([16384, 0, 0]));
    let mut bmi = asynch::Bmi088::new(i2c);
    let mut out = [0; 6];
    block_on(Device::init(&mut bmi, &mut MockDelay::new())).unwrap();
    block_on(Sensor::read(&mut bmi, &mut out)).unwrap();
    assert_eq!(out[0], 29_419);
}
