use super::*;
use lemnos_hal::mock::{MockDelay, MockI2c, block_on};

const XYZ1: u16 = 6600;
const Z1: u16 = 24000;
const Z2: i16 = 700;

fn close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() <= expected.abs() * 1e-4 + 1e-6,
        "{actual} != {expected}"
    );
}

/// A BMM150 with neutral X/Y trim (no offset, no Hall sensitivity) and
/// realistic Z trim.
fn chip() -> MockI2c {
    let mut rest = [0u8; 10];
    rest[0..2].copy_from_slice(&Z2.to_le_bytes());
    rest[2..4].copy_from_slice(&Z1.to_le_bytes());
    rest[4..6].copy_from_slice(&XYZ1.to_le_bytes());
    MockI2c::new()
        .with_target(DEFAULT_ADDRESS, AddressWidth::Bits8)
        .with_registers(DEFAULT_ADDRESS, REG_CHIP_ID, &[CHIP_ID])
        .with_registers(DEFAULT_ADDRESS, REG_TRIM_X1, &[0, 0])
        .with_registers(DEFAULT_ADDRESS, REG_TRIM_Z4, &[0, 0, 0, 0])
        .with_registers(DEFAULT_ADDRESS, REG_TRIM_Z2, &rest)
}

fn data(x: i16, y: i16, z: i16, rhall: u16) -> [u8; 8] {
    let mut bytes = [0u8; 8];
    bytes[0..2].copy_from_slice(&(x << 3).to_le_bytes());
    bytes[2..4].copy_from_slice(&(y << 3).to_le_bytes());
    bytes[4..6].copy_from_slice(&(z << 1).to_le_bytes());
    bytes[6..8].copy_from_slice(&((rhall << 2) | 1).to_le_bytes());
    bytes
}

#[test]
fn init_powers_up_reads_trim_and_configures() {
    let mut mag = Bmm150::new(chip(), DEFAULT_ADDRESS);
    let mut delay = MockDelay::new();
    let config = Config {
        preset: Preset::Enhanced,
        data_rate: DataRate::Hz30,
    };
    mag.init(&mut delay, config).unwrap();

    assert!(delay.total_ns >= 3_000_000);
    let trim = mag.trim().unwrap();
    assert_eq!((trim.xyz1, trim.z1, trim.z2), (XYZ1, Z1, Z2));
    let i2c = mag.release();
    assert_eq!(i2c.register(DEFAULT_ADDRESS, REG_POWER), 0x01);
    assert_eq!(i2c.register(DEFAULT_ADDRESS, REG_REP_XY), 0x07);
    assert_eq!(i2c.register(DEFAULT_ADDRESS, REG_REP_Z), 0x1a);
    // 30 Hz (0b111) in bits 5-3, normal mode.
    assert_eq!(i2c.register(DEFAULT_ADDRESS, REG_OP_MODE), 0x38);
}

#[test]
fn compensates_to_microtesla() {
    let i2c = chip().with_registers(DEFAULT_ADDRESS, REG_DATA, &data(160, -320, 1000, XYZ1));
    let mut mag = Bmm150::new(i2c, DEFAULT_ADDRESS);
    mag.init(&mut MockDelay::new(), Config::default()).unwrap();

    let raw = mag.read_raw().unwrap();
    assert_eq!((raw.x, raw.y, raw.z, raw.rhall), (160, -320, 1000, XYZ1));
    assert!(raw.data_ready);

    let field = mag.read().unwrap();
    // Neutral X/Y trim at rhall = xyz1: 256 × 160 / 8192 / 16 = 0.3125 µT per LSB.
    close(field.x_ut.unwrap(), 50.0);
    close(field.y_ut.unwrap(), -100.0);
    // With rhall = xyz1 and z3 = z4 = 0 the Z formula reduces to
    // raw × 2048 / (z2 + z1 × rhall / 32768).
    let gain = f32::from(Z2) + f32::from(Z1) * f32::from(XYZ1) / 32768.0;
    close(field.z_ut.unwrap(), 1000.0 * 2048.0 / gain);
}

#[test]
fn overflowed_axes_read_as_none() {
    let bytes = data(RawSample::XY_OVERFLOW, 10, RawSample::Z_OVERFLOW, XYZ1);
    let i2c = chip().with_registers(DEFAULT_ADDRESS, REG_DATA, &bytes);
    let mut mag = Bmm150::new(i2c, DEFAULT_ADDRESS);
    mag.init(&mut MockDelay::new(), Config::default()).unwrap();
    let field = mag.read().unwrap();
    assert_eq!(field.x_ut, None);
    assert!(field.y_ut.is_some());
    assert_eq!(field.z_ut, None);
}

#[test]
fn rejects_wrong_chip_and_reads_before_init() {
    let i2c = chip().with_registers(DEFAULT_ADDRESS, REG_CHIP_ID, &[0x00]);
    let mut mag = Bmm150::new(i2c, DEFAULT_ADDRESS);
    assert_eq!(mag.read().unwrap_err(), Error::NotInitialized);
    let error = mag
        .init(&mut MockDelay::new(), Config::default())
        .unwrap_err();
    assert_eq!(error, Error::WrongChip { found: 0 });
    assert_eq!(error.kind(), ErrorKind::Unsupported);
}

#[test]
fn power_down_requires_init_again() {
    let mut mag = Bmm150::new(chip(), DEFAULT_ADDRESS);
    mag.init(&mut MockDelay::new(), Config::default()).unwrap();
    mag.power_down().unwrap();
    assert_eq!(mag.read().unwrap_err(), Error::NotInitialized);
}

#[test]
fn async_driver_matches_blocking() {
    let i2c = chip().with_registers(DEFAULT_ADDRESS, REG_DATA, &data(160, 0, 0, XYZ1));
    let mut mag = asynch::Bmm150::new(i2c, DEFAULT_ADDRESS);
    block_on(mag.init(&mut MockDelay::new(), Config::default())).unwrap();
    close(block_on(mag.read()).unwrap().x_ut.unwrap(), 50.0);
}

#[test]
fn resume_reads_with_a_saved_trim() {
    let mut first = Bmm150::new(chip(), DEFAULT_ADDRESS);
    first
        .init(&mut MockDelay::new(), Config::default())
        .unwrap();
    let trim = *first.trim().unwrap();

    let i2c = first
        .release()
        .with_registers(DEFAULT_ADDRESS, REG_DATA, &data(160, 0, 0, XYZ1));
    let mut mag = Bmm150::resume(i2c, DEFAULT_ADDRESS, trim);
    close(mag.read().unwrap().x_ut.unwrap(), 50.0);
}
