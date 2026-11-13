extern crate std;

use super::*;
use crate::chip::{ConfigError, Error, INA226_CONFIG, INA238_ADC_CONFIG, INA238_ADCRANGE};
use lemnos_core::{ConfigValue, Driver, Reply, Resources};
use lemnos_hal::HalError;
use lemnos_hal::register::{AddressWidth, I2cRegisters, RegisterBus};
use lemnos_sim::{Layout, SimBoard, SimI2c};

fn close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() <= expected.abs() * 1e-4 + 1e-9,
        "{actual} != {expected}"
    );
}

/// A 16-bit register of the simulated chip at `address`, 0 if unset.
fn register(sim: &SimI2c, address: u8, register: u8) -> u16 {
    match sim.word(address, register).as_deref() {
        Some(&[high, low]) => u16::from_be_bytes([high, low]),
        _ => 0,
    }
}

/// A chip at the default address whose ID registers identify it.
fn chip_bus(sim: &SimI2c, manufacturer_register: u8, device_register: u8, device_id: u16) {
    sim.attach(chip::DEFAULT_ADDRESS, Layout::Words);
    sim.set_word(
        chip::DEFAULT_ADDRESS,
        manufacturer_register,
        &chip::MANUFACTURER_TI.to_be_bytes(),
    );
    sim.set_word(
        chip::DEFAULT_ADDRESS,
        device_register,
        &device_id.to_be_bytes(),
    );
}

fn set16(sim: &SimI2c, register: u8, value: u16) {
    sim.set_word(chip::DEFAULT_ADDRESS, register, &value.to_be_bytes());
}

const INA226: [(&str, ConfigValue<'static>); 3] = [
    ("chip", ConfigValue::Str("ina226")),
    ("shunt_ohms", ConfigValue::Float(0.01)),
    ("max_current_a", ConfigValue::Float(8.192)),
];
const INA238: [(&str, ConfigValue<'static>); 3] = [
    ("chip", ConfigValue::Str("ina238")),
    ("shunt_ohms", ConfigValue::Float(0.01)),
    ("max_current_a", ConfigValue::Float(2.0)),
];
const INA260: [(&str, ConfigValue<'static>); 1] = [("chip", ConfigValue::Str("ina260"))];

/// Binds a driver whose `bus` need is the board's `bus` I2C.
fn bind(board: &mut SimBoard, entries: &[(&str, ConfigValue<'_>)]) -> lemnos_core::Result<Ina2xx> {
    let mut resources = Resources::new(board, None);
    Ina2xx::bind(&mut resources, &Config::new(entries))
}

fn board_with_bus() -> (SimBoard, SimI2c) {
    let board = SimBoard::new();
    let sim = board.add_i2c("bus");
    (board, sim)
}

// Chip layer.

#[test]
fn fixed_readings_use_exact_integer_units() {
    let mut sim = SimI2c::default();
    chip_bus(&sim, 0xfe, 0xff, 0x2260);
    set16(&sim, 0x01, 4000);
    set16(&sim, 0x02, 9600);
    set16(&sim, 0x03, 3840);
    set16(&sim, 0x04, 16000);
    let ina = chip::Ina::new(
        chip::Model::Ina226,
        chip::DEFAULT_ADDRESS,
        chip::Calibration::from_micro(10_000, 8_192_000),
    )
    .unwrap();
    ina.init(&mut sim).unwrap();
    assert_eq!(
        ina.read_fixed(&mut sim).unwrap(),
        chip::ReadingFixed {
            bus_voltage_uv: 12_000_000,
            shunt_voltage_nv: 10_000_000,
            current_na: 4_000_000_000,
            power_nw: 24_000_000_000,
            die_temperature_mc: None,
        }
    );

    let mut sim = SimI2c::default();
    chip_bus(&sim, 0x3e, 0x3f, 0x2381);
    set16(&sim, 0x06, 200 << 4);
    set16(&sim, 0x07, -16384i16 as u16);
    sim.set_word(chip::DEFAULT_ADDRESS, 0x08, &[0x0f, 0x00, 0x00]);
    let ina = chip::Ina::new(
        chip::Model::Ina238,
        chip::DEFAULT_ADDRESS,
        chip::Calibration::from_micro(10_000, 2_000_000),
    )
    .unwrap();
    ina.init(&mut sim).unwrap();
    let reading = ina.read_fixed(&mut sim).unwrap();
    // 2 A / 2^15 = 61035.16 nA, rounded to 61035 nA.
    assert_eq!(reading.current_na, -16384 * 61_035);
    assert_eq!(reading.power_nw, 0x0f_0000 * 61_035 / 5);
    assert_eq!(reading.die_temperature_mc, Some(25_000));
    assert_eq!(register(&sim, chip::DEFAULT_ADDRESS, 0x02), 2000);
}

#[test]
fn rejects_the_wrong_chip_without_writing() {
    // An INA226 where an INA238 is configured: different ID registers and IDs.
    let mut sim = SimI2c::default();
    chip_bus(&sim, 0xfe, 0xff, 0x2260);
    let ina = chip::Ina::new(
        chip::Model::Ina238,
        chip::DEFAULT_ADDRESS,
        chip::Calibration::from_micro(10_000, 2_000_000),
    )
    .unwrap();
    let error = ina.init(&mut sim).unwrap_err();
    assert!(matches!(
        error,
        Error::WrongChip {
            model: chip::Model::Ina238,
            ..
        }
    ));
    assert_eq!(error.kind(), lemnos_hal::ErrorKind::Unsupported);
    assert_eq!(sim.word(chip::DEFAULT_ADDRESS, 0x02), None);
}

#[test]
fn rejects_configs_the_chip_cannot_calibrate() {
    use chip::{Calibration, Model};
    for (model, calibration) in [
        (Model::Ina226, Calibration::from_micro(0, 1_000_000)),
        (Model::Ina226, Calibration::from_micro(10_000, 0)),
        (Model::Ina226, Calibration::from_micro(1, 1000)),
        (Model::Ina238, Calibration::from_micro(10_000, 20_000_000)),
    ] {
        let error: ConfigError =
            chip::Ina::new(model, chip::DEFAULT_ADDRESS, calibration).unwrap_err();
        assert_eq!(
            error.kind(),
            lemnos_hal::ErrorKind::Configuration,
            "{model:?} {calibration:?}"
        );
    }
}

/// A bus holding every register the readings of `model` need, with bytes that
/// differ per register and per byte (negative values included).
fn loaded_bus(model: chip::Model) -> (SimI2c, chip::Ina) {
    use chip::{Calibration, Model};
    let (manufacturer, device, id, calibration) = match model {
        Model::Ina226 => (
            0xfe,
            0xff,
            0x2260,
            Calibration::from_micro(10_000, 8_192_000),
        ),
        Model::Ina238 => (
            0x3e,
            0x3f,
            0x2381,
            Calibration::from_micro(10_000, 2_000_000),
        ),
        Model::Ina260 => (
            0xfe,
            0xff,
            0x2270,
            Calibration::from_micro(2_000, 1_000_000),
        ),
    };
    let ina = chip::Ina::new(model, chip::DEFAULT_ADDRESS, calibration).unwrap();
    let sim = SimI2c::default();
    chip_bus(&sim, manufacturer, device, id);
    for (i, &(register, bytes)) in ina.setup.registers().iter().enumerate() {
        let mut value = [0u8; 3];
        for (k, byte) in value.iter_mut().enumerate().take(usize::from(bytes)) {
            *byte = (0xb7u8 ^ (i as u8).wrapping_mul(53) ^ (k as u8).wrapping_mul(29)) | 0x81;
        }
        sim.set_word(
            chip::DEFAULT_ADDRESS,
            register as u8,
            &value[..usize::from(bytes)],
        );
    }
    (sim, ina)
}

const MODELS: [chip::Model; 3] = [
    chip::Model::Ina226,
    chip::Model::Ina238,
    chip::Model::Ina260,
];

#[test]
fn poll_is_one_transaction_for_every_model() {
    for model in MODELS {
        let (mut sim, ina) = loaded_bus(model);
        let before = sim.transactions();
        ina.read_fixed(&mut sim).unwrap();
        assert_eq!(sim.transactions() - before, 1, "{model:?}");
    }
}

#[test]
fn poll_decodes_like_per_register_reads() {
    for model in MODELS {
        let (mut sim, ina) = loaded_bus(model);
        let burst = ina.read_fixed(&mut sim).unwrap();

        // The per-register path, on the same bus contents.
        let mut regs = I2cRegisters::new(&mut sim, chip::DEFAULT_ADDRESS, AddressWidth::Bits8);
        let mut old = [0u32; 5];
        for (slot, &(register, bytes)) in old.iter_mut().zip(ina.setup.registers()) {
            *slot = regs.read(register, bytes).unwrap();
        }
        assert_eq!(burst, ina.setup.reading_fixed(&old), "{model:?}");
        assert_ne!(burst.bus_voltage_uv, 0, "{model:?}");
    }
}

// Driver.

#[test]
fn ina226_calibrates_and_reads_si_units() {
    // 8.192 A over 10 mΩ: current LSB 250 µA, CAL = 0.00512 / (250e-6 × 0.01) = 2048.
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0xfe, 0xff, 0x2260);
    set16(&sim, 0x01, 4000);
    set16(&sim, 0x02, 9600);
    set16(&sim, 0x03, 3840);
    set16(&sim, 0x04, 16000);
    let mut driver = bind(&mut board, &INA226).unwrap();
    let reading = PowerMonitor::read(&mut driver).unwrap();
    close(reading.shunt_v, 0.010);
    close(reading.bus_v, 12.0);
    close(reading.power_w, 24.0);
    close(reading.current_a, 4.0);

    assert_eq!(register(&sim, chip::DEFAULT_ADDRESS, 0x05), 2048);
    assert_eq!(register(&sim, chip::DEFAULT_ADDRESS, 0x00), INA226_CONFIG);
}

#[test]
fn ina238_uses_low_shunt_range_when_it_fits() {
    // 2 A over 10 mΩ = 20 mV ≤ 40.96 mV: ADCRANGE=1, CAL = 819.2e6 × (2/32768) × 0.01 × 4 = 2000.
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0x3e, 0x3f, 0x2381);
    set16(&sim, 0x04, 8000);
    set16(&sim, 0x05, 3840);
    set16(&sim, 0x06, 200 << 4);
    set16(&sim, 0x07, 16384);
    sim.set_word(chip::DEFAULT_ADDRESS, 0x08, &[0x0f, 0x00, 0x00]);
    let mut driver = bind(&mut board, &INA238).unwrap();
    let reading = PowerMonitor::read(&mut driver).unwrap();
    close(reading.shunt_v, 0.010);
    close(reading.bus_v, 12.0);
    close(reading.current_a, 1.0);
    close(reading.power_w, 12.0);

    assert_eq!(register(&sim, chip::DEFAULT_ADDRESS, 0x00), INA238_ADCRANGE);
    assert_eq!(
        register(&sim, chip::DEFAULT_ADDRESS, 0x01),
        INA238_ADC_CONFIG
    );
    assert_eq!(register(&sim, chip::DEFAULT_ADDRESS, 0x02), 2000);
}

#[test]
fn ina238_keeps_wide_shunt_range_for_large_currents() {
    // 10 A over 10 mΩ = 100 mV > 40.96 mV: ADCRANGE=0, shunt LSB 5 µV.
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0x3e, 0x3f, 0x2380);
    set16(&sim, 0x04, 2000);
    let mut driver = bind(
        &mut board,
        &[
            ("chip", ConfigValue::Str("INA238")),
            ("shunt_ohms", ConfigValue::Float(0.01)),
            ("max_current_a", ConfigValue::Float(10.0)),
        ],
    )
    .unwrap();
    close(PowerMonitor::read(&mut driver).unwrap().shunt_v, 0.010);
    assert_eq!(register(&sim, chip::DEFAULT_ADDRESS, 0x00), 0);
}

#[test]
fn ina260_needs_no_calibration() {
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0xfe, 0xff, 0x2270);
    set16(&sim, 0x01, 800);
    set16(&sim, 0x02, 9600);
    set16(&sim, 0x03, 1200);
    let mut driver = bind(&mut board, &INA260).unwrap();
    let reading = PowerMonitor::read(&mut driver).unwrap();
    close(reading.current_a, 1.0);
    close(reading.bus_v, 12.0);
    close(reading.power_w, 12.0);
    close(reading.shunt_v, 0.002);
}

#[test]
fn negative_current_keeps_its_sign() {
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0xfe, 0xff, 0x2260);
    set16(&sim, 0x04, -4000i16 as u16);
    let mut driver = bind(&mut board, &INA226).unwrap();
    close(PowerMonitor::read(&mut driver).unwrap().current_a, -1.0);
}

#[test]
fn wrong_chip_is_refused_and_not_written() {
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0xfe, 0xff, 0x2260);
    let mut driver = bind(&mut board, &INA238).unwrap();
    assert_eq!(PowerMonitor::read(&mut driver), Err(Status::NOT_SUPPORTED));
    assert_eq!(sim.word(chip::DEFAULT_ADDRESS, 0x02), None);
}

#[test]
fn bad_config_is_invalid_argument() {
    let cases: [&[(&str, ConfigValue<'_>)]; 6] = [
        &[("shunt_ohms", ConfigValue::Float(0.01))],
        &[("chip", ConfigValue::Str("ina219"))],
        &[
            ("chip", ConfigValue::Str("ina226")),
            ("address", ConfigValue::Int(0x80)),
            ("shunt_ohms", ConfigValue::Float(0.01)),
            ("max_current_a", ConfigValue::Float(1.0)),
        ],
        &[
            ("chip", ConfigValue::Str("ina226")),
            ("max_current_a", ConfigValue::Float(1.0)),
        ],
        &[
            ("chip", ConfigValue::Str("ina238")),
            ("shunt_ohms", ConfigValue::Float(0.01)),
            ("max_current_a", ConfigValue::Float(20.0)),
        ],
        &[
            ("chip", ConfigValue::Str("ina226")),
            ("shunt_micro_ohms", ConfigValue::Int(-1)),
            ("max_current_micro_amps", ConfigValue::Int(1_000_000)),
        ],
    ];
    for entries in cases {
        let (mut board, _sim) = board_with_bus();
        assert_eq!(
            bind(&mut board, entries).err(),
            Some(Status::INVALID_ARGUMENT),
            "{entries:?}"
        );
    }
}

#[test]
fn absent_chip_is_retried_on_each_read_until_it_appears() {
    let (mut board, sim) = board_with_bus();
    let mut driver = bind(&mut board, &INA226).unwrap();
    assert!(matches!(Driver::health(&driver), Health::Degraded(_)));
    assert!(PowerMonitor::read(&mut driver).is_err());

    chip_bus(&sim, 0xfe, 0xff, 0x2260);
    set16(&sim, 0x02, 9600);
    set16(&sim, 0x04, 16000);
    let reading = PowerMonitor::read(&mut driver).unwrap();
    close(reading.bus_v, 12.0);
    assert_eq!(register(&sim, chip::DEFAULT_ADDRESS, 0x05), 2048);
    assert_eq!(Driver::health(&driver), Health::Ok);
}

#[test]
fn a_failed_read_is_calibrated_again_on_the_next() {
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0xfe, 0xff, 0x2260);
    let mut driver = bind(&mut board, &INA226).unwrap();
    PowerMonitor::read(&mut driver).unwrap();

    // The chip stops answering, then comes back with its calibration lost.
    sim.set_dead(chip::DEFAULT_ADDRESS, true);
    assert!(PowerMonitor::read(&mut driver).is_err());
    assert!(matches!(Driver::health(&driver), Health::Degraded(_)));
    sim.set_word(chip::DEFAULT_ADDRESS, 0x05, &[0, 0]);
    sim.set_dead(chip::DEFAULT_ADDRESS, false);

    PowerMonitor::read(&mut driver).unwrap();
    assert_eq!(register(&sim, chip::DEFAULT_ADDRESS, 0x05), 2048);
    assert_eq!(Driver::health(&driver), Health::Ok);
}

#[test]
fn describe_names_the_four_power_channels() {
    let (mut board, _sim) = board_with_bus();
    let driver = bind(&mut board, &INA226).unwrap();
    let mut out = Description::new();
    Driver::describe(&driver, &mut out);
    for (name, field) in [
        ("bus_voltage", "bus_v"),
        ("shunt_voltage", "shunt_v"),
        ("current", "current_a"),
        ("power", "power_w"),
    ] {
        assert_eq!(out.find(name).unwrap().field, Some(field), "{name}");
    }
}

#[test]
fn dispatch_serves_the_reading_and_refuses_other_methods() {
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0xfe, 0xff, 0x2260);
    set16(&sim, 0x02, 9600);
    let mut driver = bind(&mut board, &INA226).unwrap();
    let mut buf = [0u8; 32];
    let mut reply = Reply::new(&mut buf);
    let read = MethodId {
        contract: 0,
        method: 1,
    };
    assert_eq!(
        Driver::dispatch(&mut driver, read, &[], &mut reply),
        Status::OK
    );
    // A block (4), then four f32s.
    assert_eq!(reply.len(), 4 + 4 * 4);
    let other = MethodId {
        contract: 0,
        method: 9,
    };
    assert_eq!(
        Driver::dispatch(&mut driver, other, &[], &mut reply),
        Status::NOT_SUPPORTED
    );
}

// A chip the operating system's driver runs.

fn bind_host_owned(
    board: &mut SimBoard,
    entries: &[(&str, ConfigValue<'_>)],
) -> lemnos_core::Result<Ina2xx> {
    let mut resources = Resources::new(board, None);
    Ina2xx::bind(&mut resources, &Config::new(entries).for_host_owned())
}

/// Every register the simulated chip holds.
fn words(sim: &SimI2c) -> std::vec::Vec<Option<std::vec::Vec<u8>>> {
    (0..=255)
        .map(|register| sim.word(chip::DEFAULT_ADDRESS, register))
        .collect()
}

#[test]
fn a_host_owned_ina238_is_read_in_the_range_its_owner_chose_and_never_written() {
    // The owner chose the low range and a calibration of its own: -8000 × 1.25 µV = -10 mV,
    // which over 10 mΩ is -1 A, at 3840 × 3.125 mV = 12 V.
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0x3e, 0x3f, 0x2381);
    set16(&sim, 0x00, INA238_ADCRANGE);
    set16(&sim, 0x01, 0x1234);
    set16(&sim, 0x02, 777);
    set16(&sim, 0x04, -8000i16 as u16);
    set16(&sim, 0x05, 3840);
    set16(&sim, 0x07, 0x7fff);
    let before = words(&sim);
    let mut driver = bind_host_owned(&mut board, &INA238).unwrap();
    for _ in 0..3 {
        let reading = PowerMonitor::read(&mut driver).unwrap();
        close(reading.shunt_v, -0.010);
        close(reading.bus_v, 12.0);
        close(reading.current_a, -1.0);
        close(reading.power_w, 12.0);
    }
    assert!(driver.verified);
    assert_eq!(words(&sim), before, "nothing written");

    // Its owner moves it to the wide range: a new verification reads it again.
    set16(&sim, 0x00, 0);
    driver.verified = false;
    close(PowerMonitor::read(&mut driver).unwrap().shunt_v, -0.040);
}

#[test]
fn a_host_owned_ina260_reads_as_it_stands() {
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0xfe, 0xff, 0x2270);
    set16(&sim, 0x00, 0x6127);
    set16(&sim, 0x01, 800);
    set16(&sim, 0x02, 9600);
    set16(&sim, 0x03, 1200);
    let before = words(&sim);
    let mut driver = bind_host_owned(&mut board, &INA260).unwrap();
    let reading = PowerMonitor::read(&mut driver).unwrap();
    close(reading.current_a, 1.0);
    close(reading.power_w, 12.0);
    assert_eq!(words(&sim), before);
}
