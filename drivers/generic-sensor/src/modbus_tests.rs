//! The description drivers over Modbus RTU, against a made-up device that answers on the
//! simulated port, and the frames themselves.

use super::*;
use description::{Kind, ParseError};
use lemnos_core::{Driver, Resources};
use lemnos_measure::test_alloc::allocations;
use lemnos_sim::SimBoard;
use lemnos_sim::mock::MockUart;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::vec;
use std::vec::Vec;

const UNIT: u8 = 7;

#[cfg(not(feature = "std"))]
macro_rules! chip_file {
    ($name:literal) => {
        include_str!(concat!("../tests/chips/", $name))
    };
}
#[cfg(feature = "std")]
macro_rules! chip_file {
    ($name:literal) => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/chips/", $name)
    };
}

const TEMP: &str = chip_file!("sim-modbus-temp.chip");
const POWER: &str = chip_file!("sim-modbus-power.chip");
const I2C_TEMP: &str = chip_file!("sim-temp.chip");

/// What the device at the far end of the port holds, and how it misbehaves.
#[derive(Default)]
struct Device {
    holding: BTreeMap<u16, u16>,
    input: BTreeMap<u16, u16>,
    /// Answers nothing, as an unplugged device.
    gone: bool,
    /// Spoils the CRC of its next reply.
    garble: bool,
    /// Every request it understood, as (function, register).
    seen: Vec<(u8, u16)>,
}

fn frame(mut body: Vec<u8>) -> Vec<u8> {
    let crc = modbus::crc16(&body).to_le_bytes();
    body.extend_from_slice(&crc);
    body
}

impl Device {
    fn answer(&mut self, request: &[u8]) -> Vec<u8> {
        if self.gone || request.len() != 8 || request[0] != UNIT {
            return Vec::new();
        }
        if modbus::crc16(&request[..6]).to_le_bytes() != [request[6], request[7]] {
            return Vec::new();
        }
        let function = request[1];
        let register = u16::from_be_bytes([request[2], request[3]]);
        let word = u16::from_be_bytes([request[4], request[5]]);
        self.seen.push((function, register));
        let exception = |code| frame(vec![UNIT, function | 0x80, code]);
        let mut reply = match function {
            3 | 4 => {
                let table = if function == 4 {
                    &self.input
                } else {
                    &self.holding
                };
                let mut body = vec![UNIT, function, (word * 2) as u8];
                for at in register..register + word {
                    match table.get(&at) {
                        Some(value) => body.extend_from_slice(&value.to_be_bytes()),
                        None => return exception(2),
                    }
                }
                frame(body)
            }
            6 => {
                self.holding.insert(register, word);
                request.to_vec()
            }
            _ => exception(1),
        };
        if self.garble {
            self.garble = false;
            *reply.last_mut().unwrap() ^= 0xff;
        }
        reply
    }
}

/// A board with the port `uart` and the device behind it.
fn port() -> (SimBoard, MockUart, Arc<Mutex<Device>>) {
    let board = SimBoard::new();
    let uart = board.add_uart("uart");
    let device = Arc::new(Mutex::new(Device::default()));
    let far = device.clone();
    uart.respond_with(move |request| far.lock().unwrap().answer(request));
    (board, uart, device)
}

fn bind_with<D: Driver>(
    board: &mut SimBoard,
    description: &str,
    extra: &[(&str, ConfigValue<'_>)],
    host_owned: bool,
) -> Result<D> {
    let mut entries = vec![
        ("address", ConfigValue::Int(i64::from(UNIT))),
        ("description", ConfigValue::Str(description)),
    ];
    entries.extend_from_slice(extra);
    let mut config = Config::new(&entries);
    if host_owned {
        config = config.for_host_owned();
    }
    let mut res = Resources::new(board, None);
    D::bind(&mut res, &config)
}

fn bind<D: Driver>(board: &mut SimBoard, description: &str) -> Result<D> {
    bind_with(board, description, &[], false)
}

fn close(actual: f32, expected: f32) {
    let tolerance = expected.abs() * 1e-5 + 1e-9;
    assert!(
        (actual - expected).abs() <= tolerance,
        "{actual} != {expected}"
    );
}

#[test]
fn the_crc_is_the_one_the_modbus_guide_gives() {
    // Read one holding register at 0 from unit 1: the frame the serial line guide shows.
    assert_eq!(
        modbus::crc16(&[0x01, 0x03, 0x00, 0x00, 0x00, 0x01]).to_le_bytes(),
        [0x84, 0x0a]
    );
}

#[test]
fn a_temperature_probe_on_modbus_is_framed_identified_started_and_read() {
    let (mut board, uart, device) = port();
    device
        .lock()
        .unwrap()
        .input
        .extend([(0, 0x5a01), (1, (-125i16) as u16)]);
    let mut temp = bind::<ModbusTemperature>(&mut board, TEMP).unwrap();
    assert_eq!(uart.format(), None, "bind does not touch the port");

    close(temp.read().unwrap(), -12.5);
    assert_eq!(
        uart.format(),
        Some(UartFormat {
            baud: 9600,
            parity: Parity::Even,
            stop_bits: StopBits::One,
        }),
        "the Modbus default frame"
    );
    let device_state = device.lock().unwrap();
    assert_eq!(device_state.holding.get(&0x0200), Some(&1), "started");
    assert_eq!(device_state.seen, [(4, 0), (6, 0x0200), (4, 1)]);
    drop(device_state);
    // The first request on the wire, byte for byte: unit 7, function 4, register 0, one register.
    let first = frame(vec![UNIT, 4, 0, 0, 0, 1]);
    assert_eq!(&uart.sent()[..8], first.as_slice());

    device.lock().unwrap().input.insert(1, 231);
    close(temp.read().unwrap(), 23.1);
    assert_eq!(Driver::health(&temp), Health::Ok);
}

#[test]
fn the_frame_follows_the_settings() {
    let (mut board, uart, device) = port();
    device.lock().unwrap().input.extend([(0, 0x5a01), (1, 0)]);
    let mut temp = bind_with::<ModbusTemperature>(
        &mut board,
        TEMP,
        &[
            ("baud", ConfigValue::Int(19_200)),
            ("parity", ConfigValue::Str("none")),
            ("stop_bits", ConfigValue::Int(2)),
            ("response_ms", ConfigValue::Int(50)),
        ],
        false,
    )
    .unwrap();
    temp.read().unwrap();
    assert_eq!(
        uart.format(),
        Some(UartFormat {
            baud: 19_200,
            parity: Parity::None,
            stop_bits: StopBits::Two,
        })
    );
}

#[test]
fn a_power_meter_reads_values_across_two_registers_low_half_first() {
    let (mut board, _uart, device) = port();
    let current = (-2_500i32) as u32;
    let power = 70_000u32;
    device.lock().unwrap().holding.extend([
        (0x10, 2301),
        (0x11, (-150i16) as u16),
        (0x12, current as u16),
        (0x13, (current >> 16) as u16),
        (0x14, power as u16),
        (0x15, (power >> 16) as u16),
    ]);
    let mut meter = bind::<ModbusPowerMonitor>(&mut board, POWER).unwrap();
    let reading = meter.read().unwrap();
    close(reading.bus_v, 230.1);
    close(reading.shunt_v, -0.000150);
    close(reading.current_a, -2.5);
    close(reading.power_w, 7_000.0);
    let seen = device.lock().unwrap().seen.clone();
    assert_eq!(seen, [(3, 0x10), (3, 0x11), (3, 0x12), (3, 0x14)]);
}

#[test]
fn a_device_that_does_not_answer_fails_the_read_and_is_tried_again() {
    let (mut board, _uart, device) = port();
    device.lock().unwrap().input.extend([(0, 0x5a01), (1, 100)]);
    let mut temp = bind_with::<ModbusTemperature>(
        &mut board,
        TEMP,
        &[("response_ms", ConfigValue::Int(1))],
        false,
    )
    .unwrap();
    temp.read().unwrap();

    device.lock().unwrap().gone = true;
    for _ in 0..5 {
        assert_eq!(temp.read(), Err(Status::TIMED_OUT));
        assert_eq!(Driver::health(&temp), Health::Failed("bus error"));
    }
    device.lock().unwrap().gone = false;
    device.lock().unwrap().holding.clear();
    close(temp.read().unwrap(), 10.0);
    assert_eq!(
        device.lock().unwrap().holding.get(&0x0200),
        Some(&1),
        "started again"
    );
}

#[test]
fn a_garbled_reply_and_an_exception_are_reported() {
    let (mut board, _uart, device) = port();
    device.lock().unwrap().input.extend([(0, 0x5a01), (1, 100)]);
    let mut temp = bind::<ModbusTemperature>(&mut board, TEMP).unwrap();
    temp.read().unwrap();

    device.lock().unwrap().garble = true;
    assert_eq!(temp.read(), Err(Status::IO));
    assert_eq!(Driver::health(&temp), Health::Failed("garbled reply"));
    temp.read().unwrap();

    device.lock().unwrap().input.remove(&1);
    assert_eq!(temp.read(), Err(Status::NOT_SUPPORTED));
    assert_eq!(
        Driver::health(&temp),
        Health::Failed("the device refused a request")
    );
}

#[test]
fn a_wrong_device_code_is_an_identity_mismatch_with_nothing_written() {
    let (mut board, _uart, device) = port();
    device.lock().unwrap().input.extend([(0, 0x5a02), (1, 0)]);
    let mut temp = bind::<ModbusTemperature>(&mut board, TEMP).unwrap();
    assert_eq!(temp.read(), Err(Status::NOT_SUPPORTED));
    assert_eq!(Driver::health(&temp), Health::Failed("identity mismatch"));
    assert!(device.lock().unwrap().holding.is_empty());
}

#[test]
fn binding_checks_the_transport_against_what_the_board_gave() {
    // A Modbus description on an I2C driver and the other way round, and no port at all.
    let mut board = SimBoard::new();
    board.add_i2c("bus");
    assert_eq!(
        bind::<GenericTemperature>(&mut board, TEMP).err(),
        Some(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        bind::<ModbusTemperature>(&mut board, TEMP).err(),
        Some(Status::NOT_FOUND)
    );
    let (mut board, _uart, _device) = port();
    assert_eq!(
        bind::<ModbusTemperature>(&mut board, I2C_TEMP).err(),
        Some(Status::INVALID_ARGUMENT)
    );
    // Port settings on an I2C chip, and bad ones on Modbus.
    let mut board = SimBoard::new();
    board.add_i2c("bus");
    assert_eq!(
        bind_with::<GenericTemperature>(
            &mut board,
            I2C_TEMP,
            &[("baud", ConfigValue::Int(9600))],
            false
        )
        .err(),
        Some(Status::INVALID_ARGUMENT)
    );
    for extra in [
        ("baud", ConfigValue::Int(0)),
        ("parity", ConfigValue::Str("mark")),
        ("stop_bits", ConfigValue::Int(3)),
        ("response_ms", ConfigValue::Int(0)),
        ("response_ms", ConfigValue::Int(501)),
        ("address", ConfigValue::Int(0)),
        ("address", ConfigValue::Int(248)),
    ] {
        let (mut board, _uart, _device) = port();
        let mut entries = vec![("description", ConfigValue::Str(TEMP)), extra];
        if extra.0 != "address" {
            entries.push(("address", ConfigValue::Int(i64::from(UNIT))));
        }
        let mut res = Resources::new(&mut board, None);
        assert_eq!(
            ModbusTemperature::bind(&mut res, &Config::new(&entries)).err(),
            Some(Status::INVALID_ARGUMENT),
            "{extra:?}"
        );
    }
    // Reading a Modbus device means sending requests, which a host-owned device may not.
    let (mut board, _uart, _device) = port();
    assert_eq!(
        bind_with::<ModbusTemperature>(&mut board, TEMP, &[], true).err(),
        Some(Status::NOT_SUPPORTED)
    );
}

#[test]
fn modbus_lines_are_checked() {
    let error = |text: &str| Chip::parse(text, Kind::Temperature).err();
    let at = |line, reason| Some(ParseError { line, reason });
    assert_eq!(
        error("value temperature 0 16 big signed 1\nmodbus input"),
        at(2, "modbus comes before every other line")
    );
    assert_eq!(error("modbus coils"), at(1, "modbus is holding or input"));
    assert_eq!(
        error("modbus input\nvalue temperature 0x10000 16 big signed 1"),
        at(2, "a modbus register or value is above 65535")
    );
    assert_eq!(
        error("modbus input\ninit 0x10 0x10000"),
        at(2, "a modbus register or value is above 65535")
    );
    assert_eq!(
        error("modbus input\ninit 0x10 0x1 32 big"),
        at(2, "a modbus init writes one 16-bit register")
    );
    assert_eq!(
        error("modbus input\nidentity 0 0x10000"),
        at(2, "a modbus register or value is above 65535")
    );
    assert_eq!(
        error("value temperature 0x100 16 big signed 1"),
        at(1, "a register or value is above 255")
    );

    let chip = Chip::parse(
        "modbus holding\ninit 0x10 0x1234\nvalue temperature 0x300 20 little signed 1",
        Kind::Temperature,
    )
    .unwrap();
    assert_eq!(chip.transport(), Transport::Modbus { input: false });
    assert_eq!(
        chip.steps(),
        &[Step::Write {
            register: 0x10,
            value: 0x1234,
            bytes: 2,
            endian: Endian::Big,
        }]
    );
    let channel = chip.channels()[0];
    assert_eq!((channel.register, channel.bytes), (0x300, 4));
    let chip = Chip::parse(
        "modbus input\nvalue temperature 0 12 big signed 1",
        Kind::Temperature,
    )
    .unwrap();
    assert_eq!(chip.channels()[0].bytes, 2);
}

/// A port with one reply ready, made without the allocator.
struct Canned {
    reply: [u8; 9],
    len: usize,
    at: usize,
}

impl lemnos_hal::Uart for Canned {
    type Error = ErrorKind;

    fn configure(&mut self, _format: UartFormat) -> core::result::Result<(), ErrorKind> {
        Ok(())
    }

    fn write(&mut self, data: &[u8]) -> core::result::Result<usize, ErrorKind> {
        self.at = 0;
        Ok(data.len())
    }

    fn read(
        &mut self,
        out: &mut [u8],
        timeout: Duration,
    ) -> core::result::Result<usize, ErrorKind> {
        // The drain before a request reads with no wait; it finds nothing.
        if timeout.is_zero() {
            return Ok(0);
        }
        let n = out.len().min(self.len - self.at);
        out[..n].copy_from_slice(&self.reply[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

#[test]
fn a_request_and_its_reply_allocate_nothing() {
    let reply = frame(vec![UNIT, 3, 4, 0x12, 0x34, 0x56, 0x78]);
    let mut canned = Canned {
        reply: [0; 9],
        len: reply.len(),
        at: 0,
    };
    canned.reply.copy_from_slice(&reply);
    let ((), allocated) = allocations(|| {
        for _ in 0..1000 {
            let mut link = modbus::Link {
                uart: &mut canned,
                unit: UNIT,
                wait: Duration::from_millis(1),
            };
            let mut out = [0u8; 4];
            link.read(false, 0x10, &mut out).unwrap();
            assert_eq!(out, [0x12, 0x34, 0x56, 0x78]);
        }
    });
    assert_eq!(allocated, 0);
}
