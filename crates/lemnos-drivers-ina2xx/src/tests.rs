use super::*;
use lemnos_hal::mock::block_on;

extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use embedded_hal::i2c::{ErrorKind as I2cErrorKind, ErrorType, NoAcknowledgeSource, Operation};

fn close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() <= expected.abs() * 1e-4 + 1e-9,
        "{actual} != {expected}"
    );
}

/// An INA-style target: each pointer address holds one whole multi-byte
/// register (unlike `MockI2c`'s byte-per-address register file).
#[derive(Debug, Default)]
struct WordRegisters {
    registers: BTreeMap<u8, Vec<u8>>,
    pointer: u8,
}

impl WordRegisters {
    fn with(mut self, register: u8, bytes: &[u8]) -> Self {
        self.registers.insert(register, bytes.to_vec());
        self
    }

    fn with16(self, register: u8, value: u16) -> Self {
        self.with(register, &value.to_be_bytes())
    }

    fn register16(&self, register: u8) -> u16 {
        match self.registers.get(&register).map(Vec::as_slice) {
            Some([high, low]) => u16::from_be_bytes([*high, *low]),
            _ => 0,
        }
    }

    fn transact(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), I2cErrorKind> {
        if address != DEFAULT_ADDRESS {
            return Err(I2cErrorKind::NoAcknowledge(NoAcknowledgeSource::Address));
        }
        for operation in operations {
            match operation {
                Operation::Write([pointer, value @ ..]) => {
                    self.pointer = *pointer;
                    if !value.is_empty() {
                        self.registers.insert(*pointer, value.to_vec());
                    }
                }
                Operation::Write([]) => {}
                Operation::Read(buffer) => {
                    let value = self
                        .registers
                        .get(&self.pointer)
                        .cloned()
                        .unwrap_or_default();
                    for (i, byte) in buffer.iter_mut().enumerate() {
                        *byte = value.get(i).copied().unwrap_or(0);
                    }
                }
            }
        }
        Ok(())
    }
}

impl ErrorType for WordRegisters {
    type Error = I2cErrorKind;
}

impl embedded_hal::i2c::I2c for WordRegisters {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        self.transact(address, operations)
    }
}

impl embedded_hal_async::i2c::I2c for WordRegisters {
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        self.transact(address, operations)
    }
}

fn bus(manufacturer_register: u8, device_register: u8, device_id: u16) -> WordRegisters {
    WordRegisters::default()
        .with16(manufacturer_register, MANUFACTURER_TI)
        .with16(device_register, device_id)
}

#[test]
fn ina226_calibrates_and_reads_si_units() {
    // 8.192 A over 10 mΩ: current LSB 250 µA, CAL = 0.00512 / (250e-6 × 0.01) = 2048.
    let i2c = bus(0xfe, 0xff, 0x2260)
        .with16(0x01, 4000)
        .with16(0x02, 9600)
        .with16(0x03, 3840)
        .with16(0x04, 16000);
    let mut ina = Ina::new(
        i2c,
        DEFAULT_ADDRESS,
        Model::Ina226,
        Config::new(0.01, 8.192),
    )
    .unwrap();
    ina.init().unwrap();

    let reading = ina.read().unwrap();
    close(reading.shunt_voltage_v, 0.010);
    close(reading.bus_voltage_v, 12.0);
    close(reading.power_w, 24.0);
    close(reading.current_a, 4.0);
    assert_eq!(reading.die_temperature_c, None);

    let i2c = ina.release();
    assert_eq!(i2c.register16(0x05), 2048);
    assert_eq!(i2c.register16(0x00), INA226_CONFIG);
}

#[test]
fn ina238_uses_low_shunt_range_when_it_fits() {
    // 2 A over 10 mΩ = 20 mV ≤ 40.96 mV: ADCRANGE=1, CAL = 819.2e6 × (2/32768) × 0.01 × 4 = 2000.
    let i2c = bus(0x3e, 0x3f, 0x2381)
        .with16(0x04, 8000)
        .with16(0x05, 3840)
        .with16(0x06, 200 << 4)
        .with16(0x07, 16384)
        .with(0x08, &[0x0f, 0x00, 0x00]);
    let mut ina = Ina::new(i2c, DEFAULT_ADDRESS, Model::Ina238, Config::new(0.01, 2.0)).unwrap();
    ina.init().unwrap();

    let reading = ina.read().unwrap();
    close(reading.shunt_voltage_v, 0.010);
    close(reading.bus_voltage_v, 12.0);
    close(reading.current_a, 1.0);
    close(reading.power_w, 12.0);
    close(reading.die_temperature_c.unwrap(), 25.0);

    let i2c = ina.release();
    assert_eq!(i2c.register16(0x00), INA238_ADCRANGE);
    assert_eq!(i2c.register16(0x01), INA238_ADC_CONFIG);
    assert_eq!(i2c.register16(0x02), 2000);
}

#[test]
fn ina238_keeps_wide_shunt_range_for_large_currents() {
    // 10 A over 10 mΩ = 100 mV > 40.96 mV: ADCRANGE=0, shunt LSB 5 µV.
    let i2c = bus(0x3e, 0x3f, 0x2380).with16(0x04, 2000);
    let mut ina = Ina::new(i2c, DEFAULT_ADDRESS, Model::Ina238, Config::new(0.01, 10.0)).unwrap();
    ina.init().unwrap();
    close(ina.read().unwrap().shunt_voltage_v, 0.010);
    assert_eq!(ina.release().register16(0x00), 0);
}

#[test]
fn ina260_needs_no_calibration() {
    let i2c = bus(0xfe, 0xff, 0x2270)
        .with16(0x01, 800)
        .with16(0x02, 9600)
        .with16(0x03, 1200);
    let mut ina = Ina::ina260(i2c, DEFAULT_ADDRESS);
    ina.init().unwrap();
    let reading = ina.read().unwrap();
    close(reading.current_a, 1.0);
    close(reading.bus_voltage_v, 12.0);
    close(reading.power_w, 12.0);
    close(reading.shunt_voltage_v, 0.002);
}

#[test]
fn negative_current_keeps_its_sign() {
    let i2c = bus(0xfe, 0xff, 0x2260).with16(0x04, -4000i16 as u16);
    let mut ina = Ina::new(
        i2c,
        DEFAULT_ADDRESS,
        Model::Ina226,
        Config::new(0.01, 8.192),
    )
    .unwrap();
    ina.init().unwrap();
    close(ina.read().unwrap().current_a, -1.0);
}

#[test]
fn rejects_the_wrong_chip_without_writing() {
    // An INA226 where an INA238 is configured: different ID registers and IDs.
    let i2c = bus(0xfe, 0xff, 0x2260);
    let mut ina = Ina::new(i2c, DEFAULT_ADDRESS, Model::Ina238, Config::new(0.01, 2.0)).unwrap();
    let error = ina.init().unwrap_err();
    assert!(matches!(
        error,
        Error::WrongChip {
            model: Model::Ina238,
            ..
        }
    ));
    assert_eq!(error.kind(), ErrorKind::Unsupported);
    assert_eq!(ina.release().register16(0x02), 0);
}

#[test]
fn rejects_configs_the_chip_cannot_calibrate() {
    let i2c = WordRegisters::default;
    for (model, config) in [
        (Model::Ina226, Config::new(0.0, 1.0)),
        (Model::Ina226, Config::new(0.01, f32::NAN)),
        (Model::Ina226, Config::new(1e-6, 0.001)),
        (Model::Ina238, Config::new(0.01, 20.0)),
    ] {
        let error = Ina::new(i2c(), DEFAULT_ADDRESS, model, config).unwrap_err();
        assert_eq!(
            error.kind(),
            ErrorKind::Configuration,
            "{model:?} {config:?}"
        );
    }
}

#[test]
fn async_driver_matches_blocking() {
    let i2c = bus(0x3e, 0x3f, 0x2381).with16(0x07, 16384);
    let mut ina =
        asynch::Ina::new(i2c, DEFAULT_ADDRESS, Model::Ina238, Config::new(0.01, 2.0)).unwrap();
    block_on(ina.init()).unwrap();
    close(block_on(ina.read()).unwrap().current_a, 1.0);
    assert_eq!(ina.release().register16(0x02), 2000);
}
