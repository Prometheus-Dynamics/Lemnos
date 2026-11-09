//! Raw bus and line access through the driver over the simulated buses: lines, I2C transfers, SPI
//! transfers, PWM, and what a failed call leaves behind. The write policy is in `write_policy.rs`,
//! and the transfer sizes over the wire are in `wire.rs`.

mod common;

use common::{bind, client, wired};
use lemnos_contract_raw_bus::RawBus as _;
use lemnos_core::{ConfigValue, Driver, ErrorKind, Health, Status};
use lemnos_hal::raw::{LineConfig, Polarity, PwmConfig};
use lemnos_sim::SimBoard;

/// Settings that let a device write to the chip at 0x50 and nothing else on the I2C bus.
const OPEN_0X50: [(&str, ConfigValue<'static>); 2] = [
    ("writes", ConfigValue::Bool(true)),
    ("allow_i2c", ConfigValue::Int(0x50)),
];

#[test]
fn a_line_is_configured_once_at_bind_and_then_driven_and_read() {
    let mut wiring = wired();
    let mut driver = bind(
        &mut wiring.board,
        &[
            ("writes", ConfigValue::Bool(true)),
            ("line_direction", ConfigValue::Str("output")),
            ("line_initial", ConfigValue::Bool(true)),
        ],
    )
    .unwrap();
    assert_eq!(wiring.line.configs(), [LineConfig::output(true)]);
    assert_eq!(wiring.line.level(), Some(true));
    assert_eq!(client(&mut driver).line_get(), Ok(true));

    client(&mut driver).line_set(false).unwrap();
    assert_eq!(wiring.line.level(), Some(false));
    assert_eq!(client(&mut driver).line_get(), Ok(false));
    client(&mut driver).line_set(true).unwrap();
    assert!(client(&mut driver).line_get().unwrap());
}

#[test]
fn a_line_left_alone_at_bind_is_only_read() {
    let mut wiring = wired();
    let mut driver = bind(&mut wiring.board, &[]).unwrap();
    assert!(
        wiring.line.configs().is_empty(),
        "bind must not touch the line"
    );
    wiring.line.drive(true);
    assert_eq!(client(&mut driver).line_get(), Ok(true));
}

#[test]
fn an_active_low_output_reads_back_its_logical_value() {
    let mut wiring = wired();
    let mut driver = bind(
        &mut wiring.board,
        &[
            ("writes", ConfigValue::Bool(true)),
            ("line_direction", ConfigValue::Str("output")),
            ("line_active_low", ConfigValue::Bool(true)),
        ],
    )
    .unwrap();
    assert_eq!(
        wiring.line.configs(),
        [LineConfig::output(false).active_low()]
    );
    client(&mut driver).line_set(true).unwrap();
    assert_eq!(
        wiring.line.level(),
        Some(false),
        "logical high drives the pin low"
    );
    assert_eq!(client(&mut driver).line_get(), Ok(true));
}

#[test]
fn an_i2c_write_and_a_combined_read_carry_their_bytes_in_the_call() {
    let mut wiring = wired();
    let mut driver = bind(&mut wiring.board, &OPEN_0X50).unwrap();

    // A write: pointer and value in one transaction, no buffer in between.
    assert_eq!(
        client(&mut driver).i2c_transfer(0x50, &[0x20, 0x42], 0, &mut []),
        Ok(0)
    );
    assert_eq!(wiring.i2c.byte(0x50, 0x20), Some(0x42));

    // A combined write-then-read: the pointer goes out, then the bytes come back.
    let mut into = [0u8; 4];
    assert_eq!(
        client(&mut driver).i2c_transfer(0x50, &[0x10], 2, &mut into),
        Ok(2)
    );
    assert_eq!(&into[..2], [0xaa, 0xbb]);

    // A read on its own.
    assert_eq!(
        client(&mut driver).i2c_transfer(0x50, &[], 3, &mut into),
        Ok(3)
    );
}

#[test]
fn a_failed_i2c_call_reports_on_that_call_and_the_next_call_tries_again() {
    let mut wiring = wired();
    let mut driver = bind(&mut wiring.board, &OPEN_0X50).unwrap();
    wiring
        .board
        .faults("i2c")
        .unwrap()
        .fail_next(ErrorKind::Busy);

    assert_eq!(
        client(&mut driver).i2c_transfer(0x50, &[0x10], 1, &mut [0; 1]),
        Err(Status::BUSY)
    );
    assert!(matches!(Driver::health(&driver), Health::Failed(_)));

    // No give-up and no timer: the next call goes to the bus, and a success clears the mark.
    assert_eq!(
        client(&mut driver).i2c_transfer(0x50, &[0x10], 1, &mut [0; 1]),
        Ok(1)
    );
    assert_eq!(Driver::health(&driver), Health::Ok);
}

#[test]
fn a_missing_i2c_target_is_reported_as_io_and_is_found_once_it_appears() {
    let mut wiring = wired();
    let settings = [
        ("writes", ConfigValue::Bool(true)),
        ("allow_i2c", ConfigValue::Int(0x20)),
    ];
    let mut driver = bind(&mut wiring.board, &settings).unwrap();
    assert_eq!(
        client(&mut driver).i2c_transfer(0x20, &[], 1, &mut [0; 1]),
        Err(Status::IO)
    );
    assert!(matches!(Driver::health(&driver), Health::Failed(_)));
    wiring.i2c.attach(0x20, lemnos_sim::Layout::Bytes);
    assert_eq!(
        client(&mut driver).i2c_transfer(0x20, &[], 1, &mut [0; 1]),
        Ok(1)
    );
    assert_eq!(Driver::health(&driver), Health::Ok);
}

#[test]
fn spi_is_full_duplex_and_splits_unequal_lengths_into_one_chip_select() {
    let mut wiring = wired();
    let mut driver = bind(&mut wiring.board, &[("writes", ConfigValue::Bool(true))]).unwrap();

    // The JEDEC ID read: four bytes out, four back, the answer starts with the first clock.
    wiring.spi.respond(&[0xef, 0x40, 0x18]);
    let mut into = [0u8; 4];
    assert_eq!(
        client(&mut driver).spi_transfer(&[0x9f, 0, 0, 0], 4, &mut into),
        Ok(4)
    );
    assert_eq!(into, [0xef, 0x40, 0x18, 0]);
    assert_eq!(wiring.spi.written()[0], [0x9f, 0, 0, 0]);

    // More written than read: the overlap is full duplex, the rest is write only.
    wiring.spi.respond(&[0xaa, 0xbb]);
    let mut into = [0u8; 2];
    assert_eq!(
        client(&mut driver).spi_transfer(&[0x03, 0x00, 0x10], 2, &mut into),
        Ok(2)
    );
    assert_eq!(into, [0xaa, 0xbb]);
    assert_eq!(wiring.spi.written()[1], [0x03, 0x00, 0x10]);

    // Read longer than written: the overlap is full duplex, the rest is read only.
    wiring.spi.respond(&[1, 2, 3]);
    let mut into = [0u8; 3];
    assert_eq!(
        client(&mut driver).spi_transfer(&[0x55], 3, &mut into),
        Ok(3)
    );
    assert_eq!(into, [1, 2, 3]);
    assert_eq!(wiring.spi.written()[2], [0x55]);
}

#[test]
fn a_pwm_channel_takes_period_duty_and_enable_and_its_polarity_from_the_board() {
    let mut wiring = wired();
    let mut driver = bind(&mut wiring.board, &[("writes", ConfigValue::Bool(true))]).unwrap();
    client(&mut driver).pwm_set(250_000, 125_000, true).unwrap();
    assert_eq!(
        wiring.pwm.current(),
        PwmConfig {
            period_ns: 250_000,
            duty_ns: 125_000,
            polarity: Polarity::Normal,
            enabled: true,
        }
    );

    let mut wiring = wired();
    let mut driver = bind(
        &mut wiring.board,
        &[
            ("writes", ConfigValue::Bool(true)),
            ("pwm_inverted", ConfigValue::Bool(true)),
        ],
    )
    .unwrap();
    client(&mut driver)
        .pwm_set(20_000_000, 1_000_000, true)
        .unwrap();
    assert_eq!(wiring.pwm.current().polarity, Polarity::Inversed);
}

#[test]
fn a_transfer_must_do_something_and_its_reply_buffer_must_hold_its_read() {
    let mut wiring = wired();
    let mut driver = bind(&mut wiring.board, &OPEN_0X50).unwrap();
    // Nothing to send and nothing to read.
    assert_eq!(
        client(&mut driver).i2c_transfer(0x50, &[], 0, &mut []),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        client(&mut driver).spi_transfer(&[], 0, &mut []),
        Err(Status::INVALID_ARGUMENT)
    );
    // A reply buffer too small for the read is refused before the bus.
    assert_eq!(
        client(&mut driver).i2c_transfer(0x50, &[], 4, &mut [0; 2]),
        Err(Status::BUFFER_TOO_SMALL)
    );
    // Only 7-bit addresses exist.
    assert_eq!(
        client(&mut driver).i2c_transfer(0x80, &[0], 0, &mut []),
        Err(Status::INVALID_ARGUMENT)
    );
}

#[test]
fn a_driver_to_driver_call_carries_at_most_fifty_bytes_of_i2c_write_and_fifty_two_of_spi() {
    // The arguments of a local call are 64 bytes: the address (2), the write's length (4) and bytes,
    // the read length (4) and the reply capacity (4). So 50 bytes of write fit, and 51 do not.
    let mut wiring = wired();
    let mut driver = bind(&mut wiring.board, &OPEN_0X50).unwrap();
    let fits = [0x11u8; 50];
    assert_eq!(
        client(&mut driver).i2c_transfer(0x50, &fits, 0, &mut []),
        Ok(0)
    );
    let before = wiring.i2c.transactions();
    let too_big = [0x11u8; 51];
    assert!(
        client(&mut driver)
            .i2c_transfer(0x50, &too_big, 0, &mut [])
            .is_err()
    );
    assert_eq!(
        wiring.i2c.transactions(),
        before,
        "the refused arguments reach no bus"
    );

    // SPI: the length and capacity fields are the same, with no address: 52 fit, 53 do not.
    let mut wiring = wired();
    let mut driver = bind(&mut wiring.board, &[("writes", ConfigValue::Bool(true))]).unwrap();
    assert_eq!(
        client(&mut driver).spi_transfer(&[0x22u8; 52], 0, &mut []),
        Ok(0)
    );
    assert!(
        client(&mut driver)
            .spi_transfer(&[0x22u8; 53], 0, &mut [])
            .is_err()
    );
}

#[test]
fn a_device_calls_only_the_buses_it_was_given() {
    let mut board = SimBoard::new();
    board.add_i2c("i2c");
    let mut driver = bind(&mut board, &[("writes", ConfigValue::Bool(true))]).unwrap();
    assert_eq!(client(&mut driver).line_get(), Err(Status::NOT_SUPPORTED));
    assert_eq!(
        client(&mut driver).pwm_set(1, 0, false),
        Err(Status::NOT_SUPPORTED)
    );
    assert_eq!(
        client(&mut driver).spi_transfer(&[1], 0, &mut []),
        Err(Status::NOT_SUPPORTED)
    );
}

#[test]
fn a_device_with_no_bus_at_all_does_not_bind() {
    let mut board = SimBoard::new();
    assert_eq!(bind(&mut board, &[]).err(), Some(Status::NOT_FOUND));
}

#[test]
fn a_line_configured_at_bind_must_name_a_line_the_device_has() {
    let mut board = SimBoard::new();
    board.add_i2c("i2c");
    assert_eq!(
        bind(&mut board, &[("line_direction", ConfigValue::Str("input"))]).err(),
        Some(Status::NOT_FOUND)
    );
}
