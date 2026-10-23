//! Register models of the simulated chips, on a [`SimBus`]. The drivers under
//! test read and write these registers exactly as they would a chip: the
//! models keep the data registers current with the simulated physics, and
//! the calibration registers hold what the drivers wrote.
//!
//! The register maps are the ones the drivers in `lemnos-drivers-*` use, and
//! the scalings are the datasheets' (BMI088 accelerometer and gyroscope,
//! BMM150 with a neutral trim, INA238). Where a real chip is simplified:
//!
//! - the BMM150 reports its chip ID from the start (a real chip reports it
//!   only once powered, and the driver powers it before reading it, so a
//!   model that waited for the power bit would race the driver);
//! - the BMI088 accelerometer reports its data without the power-mode gate
//!   and without the dummy byte the SPI interface adds (the drivers read
//!   I2C, which has none);
//! - the INA238 reports zero current and power until its calibration is
//!   written, as a chip does.

use super::bus::{Layout, SimBus};
use super::world::Pose;

/// What a chip sees of the board at one simulated step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub pose: Pose,
    /// The board's temperature, °C.
    pub temp_c: f64,
    /// The supply voltage at the chip, V.
    pub bus_v: f64,
    /// The current through the power monitor's shunt, A.
    pub current_a: f64,
}

/// BMI088 accelerometer and gyroscope (one chip, two dies).
pub mod bmi088 {
    pub const ACCEL_CHIP_ID: u8 = 0x1e;
    pub const GYRO_CHIP_ID: u8 = 0x0f;
    pub const ACC_CHIP_ID: u8 = 0x00;
    pub const ACC_DATA: u8 = 0x12;
    pub const ACC_TEMP: u8 = 0x22;
    pub const ACC_RANGE: u8 = 0x41;
    pub const GYR_CHIP_ID: u8 = 0x00;
    pub const GYR_DATA: u8 = 0x02;
    pub const GYR_RANGE: u8 = 0x0f;
    /// Full scale in g for each `ACC_RANGE` code.
    pub const ACCEL_FULL_SCALE_G: [f64; 4] = [3.0, 6.0, 12.0, 24.0];
    /// Full scale in °/s for each `GYR_RANGE` code.
    pub const GYRO_FULL_SCALE_DPS: [f64; 5] = [2000.0, 1000.0, 500.0, 250.0, 125.0];
    pub const STANDARD_GRAVITY: f64 = 9.806_65;
    /// The accelerometer's temperature: 23 °C at 0, 0.125 °C per count (11 bits).
    pub const TEMP_OFFSET_C: f64 = 23.0;
    pub const TEMP_LSB_C: f64 = 0.125;
}

/// BMM150 magnetometer, with a neutral factory trim.
pub mod bmm150 {
    pub const CHIP_ID: u8 = 0x32;
    pub const REG_CHIP_ID: u8 = 0x40;
    pub const REG_DATA: u8 = 0x42;
    pub const REG_TRIM_X1: u8 = 0x5d;
    pub const REG_TRIM_Z4: u8 = 0x62;
    pub const REG_TRIM_Z2: u8 = 0x68;
    /// The Hall-resistance reference the trim is neutral at (14 bits).
    pub const RHALL: u16 = 0x2000;
    /// Microtesla per X/Y count: with this trim, 5/16 µT.
    pub const XY_COUNT_UT16: f64 = 5.0;
    /// Microtesla per Z count: with this trim, 1/2 µT.
    pub const Z_COUNT_UT16: f64 = 8.0;
}

/// INA238 power and current monitor (the shunt is on the chip's input).
pub mod ina238 {
    pub const REG_CONFIG: u8 = 0x00;
    pub const REG_SHUNT_CAL: u8 = 0x02;
    pub const REG_VSHUNT: u8 = 0x04;
    pub const REG_VBUS: u8 = 0x05;
    pub const REG_DIETEMP: u8 = 0x06;
    pub const REG_CURRENT: u8 = 0x07;
    /// A 24-bit register.
    pub const REG_POWER: u8 = 0x08;
    pub const REG_MANUFACTURER: u8 = 0x3e;
    pub const REG_DIE: u8 = 0x3f;
    pub const MANUFACTURER_TI: u16 = 0x5449;
    pub const DIE_ID: u16 = 0x2381;
    /// CONFIG bit 4: the low (±40.96 mV) shunt range.
    pub const ADCRANGE: u16 = 1 << 4;
    /// Bus voltage per count, V.
    pub const VBUS_LSB_V: f64 = 3.125e-3;
    /// The shunt voltage per count in the two ranges, V.
    pub const VSHUNT_LSB_V: f64 = 5.0e-6;
    pub const VSHUNT_LSB_LOW_V: f64 = 1.25e-6;
}

/// One simulated chip on a bus.
#[derive(Debug, Clone)]
pub enum Chip {
    /// The BMI088: an accelerometer and a gyroscope at two addresses.
    Bmi088 {
        bus: SimBus,
        accel: u8,
        gyro: u8,
    },
    Bmm150 {
        bus: SimBus,
        address: u8,
    },
    Ina238 {
        bus: SimBus,
        address: u8,
        /// The shunt resistance, Ω.
        shunt_ohms: f64,
    },
}

impl Chip {
    /// Puts the chip on its bus at its addresses, with the registers a chip
    /// has at power-on (identity and trim). Its data registers are then kept
    /// current by [`update`](Self::update).
    pub fn attach(self) -> Self {
        match &self {
            Self::Bmi088 { bus, accel, gyro } => {
                bus.attach(*accel, Layout::Bytes);
                bus.attach(*gyro, Layout::Bytes);
                bus.set_bytes(*accel, bmi088::ACC_CHIP_ID, &[bmi088::ACCEL_CHIP_ID]);
                bus.set_bytes(*gyro, bmi088::GYR_CHIP_ID, &[bmi088::GYRO_CHIP_ID]);
                // The reset ranges: 6 g and 2000 °/s.
                bus.set_bytes(*accel, bmi088::ACC_RANGE, &[0x01]);
                bus.set_bytes(*gyro, bmi088::GYR_RANGE, &[0x00]);
            }
            Self::Bmm150 { bus, address } => {
                bus.attach(*address, Layout::Bytes);
                bus.set_bytes(*address, bmm150::REG_CHIP_ID, &[bmm150::CHIP_ID]);
                // Neutral trim: X1, Y1, X2, Y2 zero; Z4 zero; Z2 4095, Z1 2,
                // XYZ1 = RHALL, Z3 zero, XY2 and XY1 zero.
                bus.set_bytes(*address, bmm150::REG_TRIM_X1, &[0, 0]);
                bus.set_bytes(*address, bmm150::REG_TRIM_Z4, &[0, 0, 0, 0]);
                let rhall = bmm150::RHALL.to_le_bytes();
                let z2 = 4095u16.to_le_bytes();
                let z1 = 2u16.to_le_bytes();
                bus.set_bytes(
                    *address,
                    bmm150::REG_TRIM_Z2,
                    &[z2[0], z2[1], z1[0], z1[1], rhall[0], rhall[1], 0, 0, 0, 0],
                );
            }
            Self::Ina238 { bus, address, .. } => {
                bus.attach(*address, Layout::Words);
                bus.set_word(
                    *address,
                    ina238::REG_MANUFACTURER,
                    &ina238::MANUFACTURER_TI.to_be_bytes(),
                );
                bus.set_word(*address, ina238::REG_DIE, &ina238::DIE_ID.to_be_bytes());
            }
        }
        self
    }

    /// The addresses the chip answers on (what a fault silences).
    pub fn addresses(&self) -> Vec<u8> {
        match self {
            Self::Bmi088 { accel, gyro, .. } => vec![*accel, *gyro],
            Self::Bmm150 { address, .. } | Self::Ina238 { address, .. } => vec![*address],
        }
    }

    /// The bus the chip is on.
    pub fn bus(&self) -> &SimBus {
        match self {
            Self::Bmi088 { bus, .. } | Self::Bmm150 { bus, .. } | Self::Ina238 { bus, .. } => bus,
        }
    }

    /// Writes the chip's data registers for `sample`.
    pub fn update(&self, sample: &Sample) {
        match self {
            Self::Bmi088 { bus, accel, gyro } => update_bmi088(bus, *accel, *gyro, sample),
            Self::Bmm150 { bus, address } => update_bmm150(bus, *address, sample),
            Self::Ina238 {
                bus,
                address,
                shunt_ohms,
            } => update_ina238(bus, *address, *shunt_ohms, sample),
        }
    }
}

fn update_bmi088(bus: &SimBus, accel: u8, gyro: u8, sample: &Sample) {
    use bmi088::*;
    let range = usize::from(bus.byte(accel, ACC_RANGE) & 0x03);
    let lsb = ACCEL_FULL_SCALE_G[range] * STANDARD_GRAVITY / 32768.0;
    let mut data = [0u8; 6];
    for (chunk, a) in data.chunks_mut(2).zip(sample.pose.accel) {
        chunk.copy_from_slice(&(counts(a / lsb, 32767) as i16).to_le_bytes());
    }
    bus.set_bytes(accel, ACC_DATA, &data);

    // The temperature: 11 bits, the MSBs first, then the three LSBs at bit 5.
    let raw = (((sample.temp_c - TEMP_OFFSET_C) / TEMP_LSB_C).round() as i32) & 0x7ff;
    bus.set_bytes(accel, ACC_TEMP, &[(raw >> 3) as u8, ((raw & 7) << 5) as u8]);

    let range = usize::from(bus.byte(gyro, GYR_RANGE) & 0x07).min(4);
    let full_dps = GYRO_FULL_SCALE_DPS[range];
    let mut data = [0u8; 6];
    for (chunk, w) in data.chunks_mut(2).zip(sample.pose.gyro) {
        let dps = w.to_degrees();
        chunk.copy_from_slice(&(counts(dps / full_dps * 32768.0, 32767) as i16).to_le_bytes());
    }
    bus.set_bytes(gyro, GYR_DATA, &data);
}

fn update_bmm150(bus: &SimBus, address: u8, sample: &Sample) {
    use bmm150::*;
    let [bx, by, bz] = sample.pose.field_ut;
    // 13-bit X and Y, 15-bit Z, left-aligned in 16-bit registers.
    let x = counts(bx * 16.0 / XY_COUNT_UT16, 4095);
    let y = counts(by * 16.0 / XY_COUNT_UT16, 4095);
    let z = counts(bz * 16.0 / Z_COUNT_UT16, 16383);
    let rhall_reg = (RHALL << 2) | 0x01; // data ready
    let mut data = [0u8; 8];
    data[0..2].copy_from_slice(&((x << 3) as i16).to_le_bytes());
    data[2..4].copy_from_slice(&((y << 3) as i16).to_le_bytes());
    data[4..6].copy_from_slice(&((z << 1) as i16).to_le_bytes());
    data[6..8].copy_from_slice(&rhall_reg.to_le_bytes());
    bus.set_bytes(address, REG_DATA, &data);
}

fn update_ina238(bus: &SimBus, address: u8, shunt_ohms: f64, sample: &Sample) {
    use ina238::*;
    let low = word16(bus, address, REG_CONFIG) & ADCRANGE != 0;
    let shunt_lsb = if low { VSHUNT_LSB_LOW_V } else { VSHUNT_LSB_V };
    let i = sample.current_a;
    let vshunt = counts(i * shunt_ohms / shunt_lsb, 32767) as i16;
    bus.set_word(address, REG_VSHUNT, &vshunt.to_be_bytes());
    let vbus = (sample.bus_v / VBUS_LSB_V).round().clamp(0.0, 65535.0) as u16;
    bus.set_word(address, REG_VBUS, &vbus.to_be_bytes());
    // The die temperature: 0.125 °C per count, in the top 12 bits.
    let temp = counts(sample.temp_c / 0.125, 2047) as i16;
    bus.set_word(address, REG_DIETEMP, &(temp << 4).to_be_bytes());

    // The chip's current LSB comes from its calibration: CAL = 819.2e6 ×
    // LSB × R × gain, gain 4 in the low range.
    let cal = word16(bus, address, REG_SHUNT_CAL);
    let (current, power) = if cal == 0 {
        (0i16, 0u32)
    } else {
        let gain = if low { 4.0 } else { 1.0 };
        let lsb = f64::from(cal) / (819.2e6 * shunt_ohms * gain);
        let current = counts(i / lsb, 32767) as i16;
        let power = (sample.bus_v * i / (0.2 * lsb))
            .round()
            .clamp(0.0, 16_777_215.0) as u32;
        (current, power)
    };
    bus.set_word(address, REG_CURRENT, &current.to_be_bytes());
    let p = power.to_be_bytes();
    bus.set_word(address, REG_POWER, &p[1..4]);
}

/// A 16-bit word register, big-endian (the INA238's layout).
fn word16(bus: &SimBus, address: u8, register: u8) -> u16 {
    let w = bus.word(address, register);
    u16::from_be_bytes([
        w.first().copied().unwrap_or(0),
        w.get(1).copied().unwrap_or(0),
    ])
}

/// `value` rounded to counts and clamped to ±`limit`.
fn counts(value: f64, limit: i32) -> i32 {
    value.round().clamp(f64::from(-limit), f64::from(limit)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(accel: [f64; 3], gyro: [f64; 3], field: [f64; 3]) -> Sample {
        Sample {
            pose: Pose {
                accel,
                gyro,
                field_ut: field,
            },
            temp_c: 30.0,
            bus_v: 5.0,
            current_a: 0.0,
        }
    }

    fn le16(bus: &SimBus, address: u8, register: u8) -> i16 {
        i16::from_le_bytes([bus.byte(address, register), bus.byte(address, register + 1)])
    }

    #[test]
    fn accelerometer_counts_follow_the_configured_range() {
        let bus = SimBus::default();
        let chip = Chip::Bmi088 {
            bus: bus.clone(),
            accel: 0x18,
            gyro: 0x68,
        }
        .attach();
        // 6 g is the reset range: 1 g is 32768 / 6 counts.
        chip.update(&sample(
            [0.0, 0.0, bmi088::STANDARD_GRAVITY],
            [0.0; 3],
            [0.0; 3],
        ));
        assert_eq!(i32::from(le16(&bus, 0x18, bmi088::ACC_DATA + 4)), 5461);
        // Chip IDs as the drivers verify them.
        assert_eq!(bus.byte(0x18, 0), 0x1e);
        assert_eq!(bus.byte(0x68, 0), 0x0f);
    }

    #[test]
    fn gyroscope_counts_follow_the_configured_range() {
        let bus = SimBus::default();
        let chip = Chip::Bmi088 {
            bus: bus.clone(),
            accel: 0x18,
            gyro: 0x68,
        }
        .attach();
        // The reset range is ±2000 °/s: 1000 °/s is half scale.
        chip.update(&sample(
            [0.0; 3],
            [0.0, 1000f64.to_radians(), 0.0],
            [0.0; 3],
        ));
        assert_eq!(i32::from(le16(&bus, 0x68, bmi088::GYR_DATA + 2)), 16384);
    }

    #[test]
    fn magnetometer_counts_invert_the_driver_compensation() {
        let bus = SimBus::default();
        let chip = Chip::Bmm150 {
            bus: bus.clone(),
            address: 0x10,
        }
        .attach();
        chip.update(&sample([0.0; 3], [0.0; 3], [20.0, -10.0, -40.0]));
        let data: Vec<u8> = (0..8)
            .map(|i| bus.byte(0x10, bmm150::REG_DATA + i))
            .collect();
        // 20 µT is 64 X counts (5/16 µT each), -10 µT is -32 Y counts, and
        // -40 µT is -80 Z counts (1/2 µT each), as the driver decodes them.
        let x = i16::from_le_bytes([data[0], data[1]]) >> 3;
        let y = i16::from_le_bytes([data[2], data[3]]) >> 3;
        let z = i16::from_le_bytes([data[4], data[5]]) >> 1;
        assert_eq!((x, y, z), (64, -32, -80));
    }

    #[test]
    fn power_monitor_scales_current_and_bus_voltage() {
        let bus = SimBus::default();
        let chip = Chip::Ina238 {
            bus: bus.clone(),
            address: 0x40,
            shunt_ohms: 0.01,
        }
        .attach();
        // The driver's calibration for 5 A full scale on 10 mΩ, normal range:
        // CAL = 819.2e6 × LSB × R with LSB = 5 A / 2^15.
        let lsb = 5.0 / 32768.0;
        let cal = (819.2e6 * lsb * 0.01_f64).round() as u16;
        bus.set_word(0x40, ina238::REG_SHUNT_CAL, &cal.to_be_bytes());
        let mut s = sample([0.0; 3], [0.0; 3], [0.0; 3]);
        s.bus_v = 5.0;
        s.current_a = 1.0;
        chip.update(&s);
        let current = word16(&bus, 0x40, ina238::REG_CURRENT) as i16;
        let expected = 1.0 / (f64::from(cal) / (819.2e6 * 0.01));
        assert!(
            (f64::from(current) - expected).abs() < 1.5,
            "{current} vs {expected}"
        );
        let vbus = word16(&bus, 0x40, ina238::REG_VBUS);
        assert!((f64::from(vbus) * ina238::VBUS_LSB_V - 5.0).abs() < 0.01);
    }
}
