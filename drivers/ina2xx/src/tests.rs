extern crate std;

use super::*;
use crate::chip::{ConfigError, Error, INA226_CONFIG, INA238_ADC_CONFIG, INA238_ADCRANGE};
use lemnos_contract_self_test::{Outcome, Record, SelfTest, reason};
use lemnos_core::{
    ConfigValue, Driver, JobState, Lifecycle, Reader, Reply, Resources, Status, Unit, Writer,
};
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
fn wrong_chip_is_refused_at_bind_and_not_written() {
    // An INA238 is configured, and the INA238's manufacturer register (0x3e) does not answer TI:
    // the bind's ID read refuses it, and nothing is written.
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0xfe, 0xff, 0x2260);
    assert_eq!(bind(&mut board, &INA238).err(), Some(Status::NOT_SUPPORTED));
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
fn absent_chip_is_refused_at_bind_and_bound_when_it_appears() {
    // Nothing answers at the address: the bus NACKs, as the real bus does, so the bind is refused
    // as absent, and the next bind (a retry) goes through once the chip is there.
    let (mut board, sim) = board_with_bus();
    let refused = bind(&mut board, &INA226).err();
    assert_eq!(refused, Some(Status::NO_DEVICE));

    chip_bus(&sim, 0xfe, 0xff, 0x2260);
    set16(&sim, 0x02, 9600);
    set16(&sim, 0x04, 16000);
    let mut driver = bind(&mut board, &INA226).unwrap();
    assert!(matches!(Driver::health(&driver), Health::Degraded(_)));
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
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0xfe, 0xff, 0x2260);
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

#[test]
fn the_board_gives_the_rails_whole_unit_warning_limits() {
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0xfe, 0xff, 0x2260);
    let driver = bind(
        &mut board,
        &[
            ("chip", ConfigValue::Str("ina226")),
            ("shunt_ohms", ConfigValue::Float(0.01)),
            ("max_current_a", ConfigValue::Float(2.0)),
            ("bus_warn_below_v", ConfigValue::Float(11.0)),
            ("bus_warn_above_v", ConfigValue::Float(13.0)),
            ("current_warn_above_a", ConfigValue::Float(2.0)),
        ],
    )
    .unwrap();
    let mut out = Description::new();
    Driver::describe(&driver, &mut out);
    let bus = out.find("bus_voltage").unwrap();
    assert_eq!((bus.warn_low, bus.warn_high), (Some(11), Some(13)));
    let current = out.find("current").unwrap();
    assert_eq!((current.warn_low, current.warn_high), (None, Some(2)));
    // Without them, no limit.
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0xfe, 0xff, 0x2260);
    let bare = bind(&mut board, &INA226).unwrap();
    let mut out = Description::new();
    Driver::describe(&bare, &mut out);
    assert_eq!(out.find("bus_voltage").unwrap().warn_high, None);
}

/// A fractional limit (10.8 V) is not in the descriptor: an item's limit is a whole number of its
/// unit, and a real-valued limit needs an engine change (`Item`). Until then the limit is absent,
/// which is recorded in the driver's notes, not a silent pass.
#[test]
fn a_fractional_warning_limit_is_not_in_the_descriptor() {
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0xfe, 0xff, 0x2260);
    let driver = bind(
        &mut board,
        &[
            ("chip", ConfigValue::Str("ina226")),
            ("shunt_ohms", ConfigValue::Float(0.01)),
            ("max_current_a", ConfigValue::Float(2.0)),
            ("bus_warn_below_v", ConfigValue::Float(10.8)),
        ],
    )
    .unwrap();
    let mut out = Description::new();
    Driver::describe(&driver, &mut out);
    assert_eq!(out.find("bus_voltage").unwrap().warn_low, None);
}

/// Every electrical item is a real number in its unit: exponent 0, with real limits. Exponent -9
/// (the nano-unit scale) made the renderer multiply a volt value by 1e-9, which is the bug the
/// `read` output showed as 4.5e-9 V. Fails before the fix: the exponent was -9.
/// drivers/ina2xx/src/tests.rs::the_electrical_items_are_real_numbers_in_their_unit
#[test]
fn the_electrical_items_are_real_numbers_in_their_unit() {
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0xfe, 0xff, 0x2260);
    let driver = bind(&mut board, &INA226).unwrap();
    let mut out = Description::new();
    Driver::describe(&driver, &mut out);
    for (name, unit) in [
        ("bus_voltage", Unit::Volt),
        ("shunt_voltage", Unit::Volt),
        ("current", Unit::Ampere),
        ("power", Unit::Watt),
    ] {
        let item = out.find(name).unwrap();
        assert_eq!(
            item.exponent, 0,
            "{name}: a float field is scaled by its exponent"
        );
        assert_eq!(item.unit, unit, "{name}");
        assert!(
            item.real_min.is_some() && item.real_max.is_some(),
            "{name}: a real range"
        );
    }
}

/// A bus register of 1442 counts (0x05a2) is 1442 x 3.125 mV = 4.50625 V on an INA238. The field the
/// item shows is 4.50625 V, and with the item's exponent 0 the `read` output prints 4.50625 V, not
/// 4.5e-9 V.
/// drivers/ina2xx/src/tests.rs::a_bus_register_of_1442_reads_as_4_50625_volts_with_no_scale
#[test]
fn a_bus_register_of_1442_reads_as_4_50625_volts_with_no_scale() {
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0x3e, 0x3f, 0x2381);
    set16(&sim, 0x04, 0);
    set16(&sim, 0x05, 1442);
    set16(&sim, 0x06, 0);
    set16(&sim, 0x07, 0);
    sim.set_word(chip::DEFAULT_ADDRESS, 0x08, &[0, 0, 0]);
    let mut driver = bind(&mut board, &INA238).unwrap();
    let reading = PowerMonitor::read(&mut driver).unwrap();
    close(reading.bus_v, 4.50625);
    let mut out = Description::new();
    Driver::describe(&driver, &mut out);
    let item = out.find("bus_voltage").unwrap();
    // What `lemnos-ctl read` prints: the field times 10^exponent, in the item's unit.
    let printed = f64::from(reading.bus_v) * 10f64.powi(i32::from(item.exponent));
    assert!((printed - 4.50625).abs() < 1e-5, "printed {printed} V");
}

/// The INA226 with a bus-voltage range of 11 V to 13 V, from the board file.
const RANGED: [(&str, ConfigValue<'static>); 5] = [
    ("chip", ConfigValue::Str("ina226")),
    ("shunt_ohms", ConfigValue::Float(0.01)),
    ("max_current_a", ConfigValue::Float(8.192)),
    ("self_test_bus_min_mv", ConfigValue::Int(11000)),
    ("self_test_bus_max_mv", ConfigValue::Int(13000)),
];

/// A chip at the default address that answers the registers a reading polls.
fn answering_chip(sim: &SimI2c) {
    sim.attach(chip::DEFAULT_ADDRESS, Layout::Words);
    for register in [0x01u8, 0x02, 0x03, 0x04] {
        sim.set_word(chip::DEFAULT_ADDRESS, register, &[0, 0]);
    }
}

/// Runs test `name` to its answer on the driver's tick, and reads the answer back.
fn run_test(driver: &mut Ina2xx, name: &str) -> (JobState, Option<Record>) {
    let job = SelfTest::run(driver, name, false).unwrap();
    Driver::tick(driver, 0);
    let mut buffer = [0u8; lemnos_contract_self_test::RECORD_BYTES + 8];
    let mut writer = Writer::new(&mut buffer);
    let state = Lifecycle::job(driver, job, &mut writer);
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
fn the_bus_voltage_test_passes_inside_the_board_files_range() {
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0xfe, 0xff, 0x2260);
    let mut driver = bind(&mut board, &RANGED).unwrap();
    answering_chip(&sim);
    set16(&sim, 0x02, 9600); // 12.000 V at 1.25 mV a count
    let (state, record) = run_test(&mut driver, "bus_voltage");
    assert_eq!(state, JobState::Finished(Status::OK));
    let record = record.unwrap();
    assert_eq!(record.outcome, Outcome::Pass);
    let measure = record.measures().next().unwrap();
    assert_eq!(measure.value, 12_000_000, "micro-volts");
    assert_eq!(measure.exponent, -6);
    assert_eq!(measure.unit.as_str(), "V");
    assert_eq!(
        (measure.low, measure.high),
        (Some(11_000_000), Some(13_000_000))
    );
}

#[test]
fn the_bus_voltage_test_fails_outside_the_range_with_the_value_it_read() {
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0xfe, 0xff, 0x2260);
    let mut driver = bind(&mut board, &RANGED).unwrap();
    answering_chip(&sim);
    set16(&sim, 0x02, 4800); // 6.000 V
    let (_, record) = run_test(&mut driver, "bus_voltage");
    let record = record.unwrap();
    assert_eq!(record.outcome, Outcome::Fail);
    assert_eq!(record.reason, reason::OUT_OF_RANGE);
    assert_eq!(record.measures().next().unwrap().value, 6_000_000);
}

#[test]
fn without_a_range_the_test_is_skipped_and_the_bus_is_not_touched() {
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0xfe, 0xff, 0x2260);
    let mut driver = bind(&mut board, &INA226).unwrap();
    // The tick verifies the chip, as the host's would; the skipped test then reads nothing more.
    Driver::tick(&mut driver, 0);
    let before = sim.transactions();
    let (_, record) = run_test(&mut driver, "bus_voltage");
    let record = record.unwrap();
    assert_eq!(record.outcome, Outcome::Skipped);
    assert_eq!(record.reason, reason::NOT_CONFIGURED);
    assert_eq!(sim.transactions(), before, "a skipped test reads nothing");
}

#[test]
fn a_chip_that_does_not_answer_is_no_response() {
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0xfe, 0xff, 0x2260);
    let mut driver = bind(&mut board, &RANGED).unwrap();
    answering_chip(&sim);
    sim.set_dead(chip::DEFAULT_ADDRESS, true);
    let before = Driver::health(&driver);
    let (_, record) = run_test(&mut driver, "bus_voltage");
    assert_eq!(record.unwrap().reason, reason::NO_RESPONSE);
    assert_eq!(Driver::health(&driver), before);
}

#[test]
fn a_range_with_its_bounds_reversed_is_refused_at_bind() {
    let (mut board, _sim) = board_with_bus();
    let reversed = [
        ("chip", ConfigValue::Str("ina226")),
        ("shunt_ohms", ConfigValue::Float(0.01)),
        ("max_current_a", ConfigValue::Float(8.192)),
        ("self_test_bus_min_mv", ConfigValue::Int(13000)),
        ("self_test_bus_max_mv", ConfigValue::Int(11000)),
    ];
    assert_eq!(
        bind(&mut board, &reversed).err(),
        Some(Status::INVALID_ARGUMENT)
    );
}

#[test]
fn describe_offers_the_self_test_action() {
    let (mut board, sim) = board_with_bus();
    chip_bus(&sim, 0xfe, 0xff, 0x2260);
    let driver = bind(&mut board, &INA226).unwrap();
    let mut out = Description::new();
    Driver::describe(&driver, &mut out);
    assert_eq!(
        out.find("self_test").unwrap().kind,
        lemnos_core::ItemKind::Action
    );
}

/// The refusal `check_config` gives for `entries`, or `None` when it accepts them.
fn refusal<'a>(entries: &'a [(&'a str, ConfigValue<'a>)]) -> Option<lemnos_core::ConfigError<'a>> {
    <Ina2xx as lemnos_core::Lifecycle>::check_config(&Config::new(entries)).err()
}

#[test]
fn check_config_accepts_a_full_config() {
    let entries = [
        ("chip", ConfigValue::Str("INA226")),
        ("address", ConfigValue::Int(0x41)),
        ("shunt_ohms", ConfigValue::Float(0.01)),
        ("max_current_a", ConfigValue::Float(2.0)),
        ("bus_warn_below_v", ConfigValue::Float(10.8)),
        ("bus_warn_above_v", ConfigValue::Float(13.5)),
        ("current_warn_above_a", ConfigValue::Float(1.5)),
        ("self_test_bus_min_mv", ConfigValue::Int(11_000)),
        ("self_test_bus_max_mv", ConfigValue::Int(13_000)),
    ];
    assert_eq!(refusal(&entries), None);
}

#[test]
fn check_config_refuses_an_unknown_key_by_name_and_suggests_the_near_one() {
    let entries = [
        ("chip", ConfigValue::Str("ina226")),
        ("shunt_ohm", ConfigValue::Float(0.01)),
    ];
    let err = refusal(&entries).unwrap();
    assert_eq!(err.key, "shunt_ohm");
    assert_eq!(err.suggestion, Some("shunt_ohms"));
    let bogus = [
        ("chip", ConfigValue::Str("ina226")),
        ("bogus", ConfigValue::Int(1)),
    ];
    assert_eq!(refusal(&bogus).unwrap().key, "bogus");
}

#[test]
fn check_config_refuses_a_wrong_kind_and_a_missing_chip() {
    let wrong = [
        ("chip", ConfigValue::Str("ina226")),
        ("address", ConfigValue::Str("0x40")),
    ];
    assert_eq!(refusal(&wrong).unwrap().key, "address");
    let missing = [("shunt_ohms", ConfigValue::Float(0.01))];
    assert_eq!(refusal(&missing).unwrap().key, "chip");
}

#[test]
fn check_config_refuses_an_address_outside_seven_bits_and_accepts_the_edges() {
    let chip = ("chip", ConfigValue::Str("ina226"));
    let shunt = ("shunt_ohms", ConfigValue::Float(0.01));
    let max = ("max_current_a", ConfigValue::Float(1.0));
    assert_eq!(
        refusal(&[chip, shunt, max, ("address", ConfigValue::Int(128))])
            .unwrap()
            .key,
        "address"
    );
    assert_eq!(
        refusal(&[chip, shunt, max, ("address", ConfigValue::Int(-1))])
            .unwrap()
            .key,
        "address"
    );
    assert_eq!(
        refusal(&[chip, shunt, max, ("address", ConfigValue::Int(0))]),
        None
    );
    assert_eq!(
        refusal(&[chip, shunt, max, ("address", ConfigValue::Int(127))]),
        None
    );
}

#[test]
fn check_config_refuses_a_calibration_the_chip_cannot_take() {
    let chip = ("chip", ConfigValue::Str("ina226"));
    let zero_shunt = [
        chip,
        ("shunt_micro_ohms", ConfigValue::Int(-1)),
        ("max_current_micro_amps", ConfigValue::Int(1_000_000)),
    ];
    assert_eq!(refusal(&zero_shunt).unwrap().key, "shunt_micro_ohms");
    let fine = [
        chip,
        ("shunt_ohms", ConfigValue::Float(0.01)),
        ("max_current_a", ConfigValue::Float(1.0)),
    ];
    assert_eq!(refusal(&fine), None);
}

#[test]
fn check_config_refuses_a_warning_limit_that_is_not_a_number() {
    let entries = [
        ("chip", ConfigValue::Str("ina260")),
        ("bus_warn_above_v", ConfigValue::Float(f64::NAN)),
    ];
    assert_eq!(refusal(&entries).unwrap().key, "bus_warn_above_v");
}

#[test]
fn check_config_refuses_a_bus_range_that_is_backwards_and_accepts_equal() {
    let chip = ("chip", ConfigValue::Str("ina260"));
    let backwards = [
        chip,
        ("self_test_bus_min_mv", ConfigValue::Int(13_000)),
        ("self_test_bus_max_mv", ConfigValue::Int(11_000)),
    ];
    assert_eq!(refusal(&backwards).unwrap().key, "self_test_bus_min_mv");
    let equal = [
        chip,
        ("self_test_bus_min_mv", ConfigValue::Int(12_000)),
        ("self_test_bus_max_mv", ConfigValue::Int(12_000)),
    ];
    assert_eq!(refusal(&equal), None);
}
