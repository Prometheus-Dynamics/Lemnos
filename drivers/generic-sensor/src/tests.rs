//! The description driver on a simulated bus, next to the hand-written BMM150 on the same bus.

use super::*;
use description::{Kind, MAX_STEPS, ParseError};
use lemnos_core::{Description, Driver, Resources};
use lemnos_driver_bmm150::{Bmm150, DEFAULT_ADDRESS as BMM150_ADDRESS};
use lemnos_hal::register::Endian;
use lemnos_measure::test_alloc::{Counting, allocations};
use lemnos_sim::{Layout, SimBoard, SimI2c};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The chip the file describes, at the address the board wires it to.
const ADDRESS: u8 = 0x2c;

/// The file's text, as the driver gets it without `std`.
const TEXT: &str = include_str!("../tests/chips/sim-mag.chip");

/// The `description` setting: the text itself, in every build.
const DESCRIPTION: &str = TEXT;

fn bus() -> (SimBoard, SimI2c) {
    let board = SimBoard::new();
    let sim = board.add_i2c("bus");
    (board, sim)
}

/// The chip in the file: its identity register.
fn chip(sim: &SimI2c, identity: u8) {
    sim.attach(ADDRESS, Layout::Bytes);
    sim.set_bytes(ADDRESS, 0x00, &[identity]);
}

/// Sets the chip's three axes, as its registers hold them.
fn set_field(sim: &SimI2c, x: i16, y: i16, z: i32) {
    sim.set_bytes(ADDRESS, 0x01, &x.to_le_bytes());
    sim.set_bytes(ADDRESS, 0x03, &y.to_be_bytes());
    sim.set_bytes(ADDRESS, 0x05, &z.to_le_bytes()[..3]);
}

/// Binds the description driver to `board` with the given settings.
fn bind_with(
    board: &mut SimBoard,
    entries: &[(&str, ConfigValue<'_>)],
) -> Result<GenericMagnetometer> {
    let config = Config::new(entries);
    let mut res = Resources::new(board, None);
    GenericMagnetometer::bind(&mut res, &config)
}

fn bind(board: &mut SimBoard, address: i64, description: &str) -> Result<GenericMagnetometer> {
    bind_with(
        board,
        &[
            ("address", ConfigValue::Int(address)),
            ("description", ConfigValue::Str(description)),
        ],
    )
}

/// The hand-written BMM150 at its default address, with its chip ID and factory trim in place.
fn bmm150(board: &mut SimBoard, sim: &SimI2c) -> Bmm150 {
    sim.attach(BMM150_ADDRESS, Layout::Bytes);
    sim.set_bytes(BMM150_ADDRESS, 0x40, &[0x32]);
    sim.set_bytes(BMM150_ADDRESS, 0x5d, &[0, 0]);
    sim.set_bytes(BMM150_ADDRESS, 0x62, &[0, 0, 0, 0]);
    // Z2 = 700, Z1 = 24000, XYZ1 = 6600, the rest zero.
    sim.set_bytes(
        BMM150_ADDRESS,
        0x68,
        &[0xbc, 0x02, 0xc0, 0x5d, 0x08, 0x19, 0, 0, 0, 0],
    );
    // Data ready, Hall resistance 6600 (the bmm150 driver's own test values).
    sim.set_bytes(BMM150_ADDRESS, 0x42, &[0, 0, 0, 0, 0, 0, 0x21, 0x67]);
    let mut res = Resources::new(board, None);
    Bmm150::bind(&mut res, &Config::new(&[])).unwrap()
}

/// Checks a value in the contract's unit, to a relative tolerance.
fn close(actual: f32, expected: f32) {
    let tolerance = expected.abs() * 1e-5 + 1e-12;
    assert!(
        (actual - expected).abs() <= tolerance,
        "{actual} != {expected}"
    );
}

/// Starts the chip: the first read answers AGAIN and runs start-up to its `wait`; the tick passes
/// the wait; the next read finishes start-up and measures.
fn start(mag: &mut GenericMagnetometer) -> Vector3 {
    assert_eq!(mag.read(), Err(Status::AGAIN));
    let at = mag.sensor.now_ns + 2_000_000;
    assert_eq!(Driver::tick(mag, at), None);
    mag.read().unwrap()
}

#[test]
fn second_sensor_from_a_file_reads_what_the_chip_holds_beside_the_first() {
    let (mut board, sim) = bus();
    chip(&sim, 0x48);
    set_field(&sim, 500, -250, -70_000);
    let mut compass = bmm150(&mut board, &sim);
    let mut mag = bind(&mut board, i64::from(ADDRESS), DESCRIPTION).unwrap();

    // The file's wait holds the start-up to 2 ms; the first write lands before it.
    assert_eq!(mag.read(), Err(Status::AGAIN));
    assert_eq!(sim.byte(ADDRESS, 0x0a), Some(0x80));
    assert_eq!(sim.byte(ADDRESS, 0x09), None);
    assert_eq!(Driver::tick(&mut mag, 1_000_000), Some(2_000_000));
    assert_eq!(mag.read(), Err(Status::AGAIN));
    assert_eq!(sim.byte(ADDRESS, 0x09), None);
    assert_eq!(Driver::tick(&mut mag, 2_000_000), None);

    let field = mag.read().unwrap();
    assert_eq!(sim.byte(ADDRESS, 0x09), Some(0x1d));
    close(field.x, 500e-6);
    close(field.y, -250e-6);
    close(field.z, -70_000e-6);
    assert_eq!(Driver::health(&mag), Health::Ok);

    // The first sensor on the same bus is unaffected.
    assert_eq!(compass.read(), Err(Status::AGAIN));
    assert!(compass.read().is_ok());
    assert_eq!(Driver::health(&compass), Health::Ok);
}

#[test]
fn the_values_follow_the_chip_registers() {
    let (mut board, sim) = bus();
    chip(&sim, 0x48);
    set_field(&sim, -32_768, 32_767, 8_388_607);
    let mut mag = bind(&mut board, i64::from(ADDRESS), DESCRIPTION).unwrap();
    let field = start(&mut mag);
    close(field.x, -32_768e-6);
    close(field.y, 32_767e-6);
    close(field.z, 8_388_607e-6);

    set_field(&sim, 1, 2, -3);
    let field = mag.read().unwrap();
    close(field.x, 1e-6);
    close(field.y, 2e-6);
    close(field.z, -3e-6);
}

#[test]
fn describes_three_axes_in_tesla() {
    let (mut board, _sim) = bus();
    let mag = bind(&mut board, i64::from(ADDRESS), DESCRIPTION).unwrap();
    let mut out = Description::new();
    Driver::describe(&mag, &mut out);
    assert_eq!(out.items().count(), 3);
    let item = out.find("magnetic_field.y").unwrap();
    assert_eq!(item.unit, Unit::Tesla);
    assert_eq!(item.field, Some("y"));
}

#[test]
fn wrong_identity_is_reported_then_checked_again() {
    let (mut board, sim) = bus();
    chip(&sim, 0x00);
    set_field(&sim, 0, 0, 0);
    let mut mag = bind(&mut board, i64::from(ADDRESS), DESCRIPTION).unwrap();
    assert_eq!(mag.read(), Err(Status::NOT_SUPPORTED));
    assert_eq!(Driver::health(&mag), Health::Failed("identity mismatch"));
    assert_eq!(
        sim.byte(ADDRESS, 0x0a),
        None,
        "no start-up write before the identity"
    );

    sim.set_bytes(ADDRESS, 0x00, &[0x48]);
    start(&mut mag);
    assert_eq!(Driver::health(&mag), Health::Ok);
}

#[test]
fn bus_fault_restarts_the_chip_on_the_next_read_and_is_never_given_up() {
    let (mut board, sim) = bus();
    chip(&sim, 0x48);
    set_field(&sim, 10, 20, 30);
    let mut mag = bind(&mut board, i64::from(ADDRESS), DESCRIPTION).unwrap();
    start(&mut mag);

    sim.set_dead(ADDRESS, true);
    for _ in 0..20 {
        assert_eq!(mag.read(), Err(Status::IO));
        assert_eq!(Driver::health(&mag), Health::Failed("bus error"));
    }

    sim.set_dead(ADDRESS, false);
    sim.set_bytes(ADDRESS, 0x09, &[0]);
    start(&mut mag);
    assert_eq!(sim.byte(ADDRESS, 0x09), Some(0x1d));
    assert_eq!(Driver::health(&mag), Health::Ok);
}

/// The check the host runs on the device's settings, before any bus is touched.
fn checked<'e>(
    entries: &'e [(&'e str, ConfigValue<'e>)],
) -> core::result::Result<(), (&'e str, &'static str)> {
    <GenericMagnetometer as Lifecycle>::check_config(&Config::new(entries))
        .map_err(|error| (error.key, error.reason))
}

#[test]
fn a_description_that_does_not_parse_is_refused_by_the_check_and_at_bind() {
    let (mut board, _sim) = bus();
    let cases = [
        "frobnicate 1",
        "axis w 0x01 16 little signed 1",
        "axis x 0x01 33 little signed 1\naxis y 0 8 little signed 1\naxis z 0 8 little signed 1",
        "axis x 0x01 16 middle signed 1\naxis y 0 8 little signed 1\naxis z 0 8 little signed 1",
        "axis x 0x01 16 little signed nan\naxis y 0 8 little signed 1\naxis z 0 8 little signed 1",
        "axis x 0x01 16 little signed 1",
        "init 0x100 0x00",
        "wait 1 2",
    ];
    for text in cases {
        let entries = [
            ("address", ConfigValue::Int(i64::from(ADDRESS))),
            ("description", ConfigValue::Str(text)),
        ];
        assert_eq!(
            checked(&entries),
            Err((
                "description",
                "is not a chip description: a file's path goes in description_file"
            )),
            "{text}"
        );
        assert_eq!(
            bind(&mut board, i64::from(ADDRESS), text).err(),
            Some(Status::INVALID_ARGUMENT),
            "{text}"
        );
    }
}

#[test]
fn a_path_under_description_is_refused_and_names_description_file() {
    let entries = [
        ("address", ConfigValue::Int(i64::from(ADDRESS))),
        (
            "description",
            ConfigValue::Str("/etc/lemnos/chips/sim-mag.chip"),
        ),
    ];
    let error =
        <GenericMagnetometer as Lifecycle>::check_config(&Config::new(&entries)).unwrap_err();
    assert_eq!(error.key, "description");
    assert_eq!(
        error.reason,
        "is not a chip description: a file's path goes in description_file"
    );
}

#[test]
fn settings_are_checked_at_bind() {
    let (mut board, _sim) = bus();
    let cases: [&[(&str, ConfigValue<'_>)]; 5] = [
        &[("description", ConfigValue::Str(DESCRIPTION))],
        &[
            ("address", ConfigValue::Int(0x80)),
            ("description", ConfigValue::Str(DESCRIPTION)),
        ],
        &[
            ("address", ConfigValue::Str("0x2c")),
            ("description", ConfigValue::Str(DESCRIPTION)),
        ],
        &[("address", ConfigValue::Int(0x2c))],
        &[
            ("address", ConfigValue::Int(0x2c)),
            ("description", ConfigValue::Int(1)),
        ],
    ];
    for entries in cases {
        assert_eq!(
            bind_with(&mut board, entries).err(),
            Some(Status::INVALID_ARGUMENT)
        );
    }
    // A valid config on a board with no bus: the bus is what is missing.
    assert_eq!(
        bind(&mut SimBoard::new(), i64::from(ADDRESS), DESCRIPTION).err(),
        Some(Status::NOT_FOUND)
    );
}

#[test]
fn settings_are_checked_before_the_device_is_made() {
    assert_eq!(
        checked(&[("description", ConfigValue::Str(DESCRIPTION))]),
        Err(("address", "is required"))
    );
    assert_eq!(
        checked(&[
            ("address", ConfigValue::Int(0x80)),
            ("description", ConfigValue::Str(DESCRIPTION)),
        ]),
        Err(("address", "must be a 7-bit I2C address, 0x08 to 0x77"))
    );
    assert_eq!(
        checked(&[
            ("address", ConfigValue::Int(0x2c)),
            ("descripton", ConfigValue::Str(DESCRIPTION)),
        ]),
        Err(("descripton", "is not a setting this driver reads"))
    );
    assert_eq!(
        checked(&[
            ("address", ConfigValue::Int(0x2c)),
            ("baud", ConfigValue::Int(9600)),
            ("description", ConfigValue::Str(DESCRIPTION)),
        ]),
        Err(("baud", "is a Modbus setting, and this chip is on I2C"))
    );
    assert_eq!(
        checked(&[
            ("address", ConfigValue::Int(0x2c)),
            ("description", ConfigValue::Str(DESCRIPTION)),
        ]),
        Ok(())
    );
}

#[test]
fn the_chip_file_parses_to_its_facts() {
    let chip = Chip::parse(TEXT, Kind::Magnetometer).unwrap();
    assert_eq!(chip.identity(), Some((0x00, 0x48)));
    assert_eq!(
        chip.steps(),
        &[
            Step::Write {
                register: 0x0a,
                value: 0x80,
                bytes: 1,
                endian: Endian::Big,
            },
            Step::Wait { millis: 2 },
            Step::Write {
                register: 0x09,
                value: 0x1d,
                bytes: 1,
                endian: Endian::Big,
            },
        ]
    );
    let [x, y, z] = <[_; 3]>::try_from(chip.channels()).unwrap();
    assert_eq!(
        (x.register, x.bytes, x.endian, x.signed),
        (0x01, 2, Endian::Little, true)
    );
    assert_eq!(
        (y.register, y.bytes, y.endian, y.signed),
        (0x03, 2, Endian::Big, true)
    );
    assert_eq!(
        (z.register, z.bytes, z.endian, z.signed),
        (0x05, 3, Endian::Little, true)
    );
    close(z.scale, 1e-6);
}

#[test]
fn parse_errors_name_the_line() {
    let cases: [(&str, ParseError); 9] = [
        (
            "frobnicate 1",
            ParseError {
                line: 1,
                reason: "unknown line",
            },
        ),
        (
            "axis w 0x01 16 little signed 1",
            ParseError {
                line: 1,
                reason: "axis is x, y or z",
            },
        ),
        (
            "init 0x100 0x00",
            ParseError {
                line: 1,
                reason: "a register or value is above 255",
            },
        ),
        (
            "axis x 0x01 16 middle signed 1",
            ParseError {
                line: 1,
                reason: "endian is little or big",
            },
        ),
        (
            "axis x 0x01 16 little signed nan",
            ParseError {
                line: 1,
                reason: "scale or offset is not a finite number",
            },
        ),
        (
            "wait 1 2",
            ParseError {
                line: 1,
                reason: "extra text at the end of the line",
            },
        ),
        (
            "identity 0x00 0x48\nidentity 0x00 0x48",
            ParseError {
                line: 2,
                reason: "identity given twice",
            },
        ),
        (
            "axis x 0x01 16 little signed 1\naxis x 0x01 16 little signed 1",
            ParseError {
                line: 2,
                reason: "axis given twice",
            },
        ),
        (
            "wait 1 # a comment is not text\nwait 0xzz",
            ParseError {
                line: 2,
                reason: "not a number",
            },
        ),
    ];
    for (text, expected) in cases {
        assert_eq!(
            Chip::parse(text, Kind::Magnetometer).err(),
            Some(expected),
            "{text}"
        );
    }
    assert_eq!(
        Chip::parse(
            &"init 0x01 0x01\n".repeat(MAX_STEPS + 1),
            Kind::Magnetometer
        )
        .err(),
        Some(ParseError {
            line: MAX_STEPS + 1,
            reason: "too many init and wait lines",
        })
    );
    assert_eq!(
        Chip::parse("init 0x01 0x01\n", Kind::Magnetometer).err(),
        Some(ParseError {
            line: 0,
            reason: "axis x, y and z are each needed",
        })
    );
}

#[test]
fn numbers_are_decimal_or_hex_and_axes_decode_their_width() {
    let chip = Chip::parse(
        "axis x 0x10 8 little unsigned 1\naxis y 16 16 little signed 1\naxis z 0 32 big unsigned 1\n",
        Kind::Magnetometer,
    )
    .unwrap();
    let [x, y, z] = <[_; 3]>::try_from(chip.channels()).unwrap();
    assert_eq!(x.register, 0x10);
    assert_eq!(y.register, 16);
    assert_eq!(x.units(0xff), 255.0);
    assert_eq!(y.units(0xffff), -1.0);
    assert_eq!(z.units(0x8000_0000), 2_147_483_648.0);

    let signed24 = Chip::parse(
        "axis x 0 24 little signed 1\naxis y 0 8 little signed 1\naxis z 0 8 little signed 1\n",
        Kind::Magnetometer,
    )
    .unwrap();
    assert_eq!(signed24.channels()[0].units(0xff_ff_fe), -2.0);
}

/// Binds with the given settings, the `description_file` one among them.
fn bind_file(board: &mut SimBoard, path: &str) -> Result<GenericMagnetometer> {
    bind_with(
        board,
        &[
            ("address", ConfigValue::Int(i64::from(ADDRESS))),
            ("description_file", ConfigValue::Str(path)),
        ],
    )
}

/// The same text, the same meaning, in every build: its items, and a read of the chip's
/// registers. This runs with and without `std`; the assertions do not change.
#[test]
fn the_same_description_text_means_the_same_in_every_build() {
    let (mut board, sim) = bus();
    chip(&sim, 0x48);
    set_field(&sim, 500, -250, -70_000);
    let mut mag = bind(&mut board, i64::from(ADDRESS), TEXT).unwrap();
    start(&mut mag);
    let value = mag.read().unwrap();
    close(value.x, 500e-6);
    close(value.y, -250e-6);
    close(value.z, -70_000e-6);
    let mut out = Description::new();
    Driver::describe(&mag, &mut out);
    assert_eq!(out.items().count(), 3);
    assert_eq!(out.find("magnetic_field.x").unwrap().field, Some("x"));
}

/// `description_file` is a path, so a build without a filesystem refuses it by name: the driver
/// binds, its health names the setting, and it never reads the path as text or touches the bus.
#[cfg(not(feature = "std"))]
#[test]
fn description_file_is_refused_by_name_without_a_filesystem() {
    let (mut board, sim) = bus();
    chip(&sim, 0x48);
    let path = "/etc/lemnos/chips/sim-mag.chip";
    assert_eq!(
        checked(&[
            ("address", ConfigValue::Int(i64::from(ADDRESS))),
            ("description_file", ConfigValue::Str(path)),
        ]),
        Err((
            "description_file",
            "needs a build with a filesystem: give the chip's text as description"
        ))
    );
    assert_eq!(
        bind_file(&mut board, path).err(),
        Some(Status::INVALID_ARGUMENT)
    );
    assert_eq!(sim.byte(ADDRESS, 0x00), Some(0x48), "the chip is untouched");
}

/// With a filesystem, `description_file` reads the file once at bind, and the chip is the same
/// chip the text describes.
#[cfg(feature = "std")]
#[test]
fn description_file_reads_the_same_chip_as_its_text() {
    let (mut board, sim) = bus();
    chip(&sim, 0x48);
    set_field(&sim, 500, -250, -70_000);
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/chips/sim-mag.chip");
    let mut from_file = bind_file(&mut board, path).unwrap();
    start(&mut from_file);
    let mut from_text = bind(&mut board, i64::from(ADDRESS), TEXT).unwrap();
    start(&mut from_text);
    assert_eq!(from_file.read().unwrap(), from_text.read().unwrap());
}

#[cfg(feature = "std")]
#[test]
fn a_missing_description_file_is_refused_by_the_check() {
    let (mut board, _sim) = bus();
    let missing = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/chips/none.chip");
    assert_eq!(
        checked(&[
            ("address", ConfigValue::Int(i64::from(ADDRESS))),
            ("description_file", ConfigValue::Str(missing)),
        ]),
        Err(("description_file", "names no file: check the path"))
    );
    assert_eq!(
        bind_file(&mut board, missing).err(),
        Some(Status::INVALID_ARGUMENT)
    );
}

/// Both keys name the chip twice, so both are refused by name, in every build, before the device
/// is made.
#[test]
fn both_description_keys_are_refused_by_name() {
    let (mut board, _sim) = bus();
    let entries = [
        ("address", ConfigValue::Int(i64::from(ADDRESS))),
        ("description", ConfigValue::Str(TEXT)),
        (
            "description_file",
            ConfigValue::Str("/etc/lemnos/chips/x.chip"),
        ),
    ];
    assert_eq!(
        checked(&entries),
        Err((
            "description_file",
            "is given with description: give one of them, not both"
        ))
    );
    assert_eq!(
        bind_with(&mut board, &entries).err(),
        Some(Status::INVALID_ARGUMENT)
    );
}

#[test]
fn steady_state_reads_allocate_nothing() {
    let (mut board, sim) = bus();
    chip(&sim, 0x48);
    set_field(&sim, 500, -250, -70_000);
    let mut mag = bind(&mut board, i64::from(ADDRESS), DESCRIPTION).unwrap();
    start(&mut mag);
    let ((), allocated) = allocations(|| {
        for _ in 0..1000 {
            mag.read().unwrap();
        }
    });
    assert_eq!(allocated, 0, "reads of a started chip");
}
