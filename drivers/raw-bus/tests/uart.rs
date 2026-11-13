//! The UART through the driver over the simulated port: its frame set at bind, bytes both ways,
//! the write policy, the read's limits, and a failed call.

mod common;

use common::{all_buses_down, bind, client, wired};
use lemnos_contract_raw_bus::{RawBus as _, UART_MAX_WAIT_MS};
use lemnos_core::{ConfigValue, Driver, Health, Status};
use lemnos_hal::{Parity, StopBits, UartFormat};
use lemnos_sim::SimBoard;

const WRITES: [(&str, ConfigValue<'static>); 1] = [("writes", ConfigValue::Bool(true))];

#[test]
fn the_frame_is_set_once_at_bind_and_left_alone_without_settings() {
    let mut wiring = wired();
    bind(&mut wiring.board, &[]).unwrap();
    assert_eq!(wiring.uart.format(), None, "bind must not touch the port");

    let mut wiring = wired();
    bind(
        &mut wiring.board,
        &[
            ("uart_baud", ConfigValue::Int(9600)),
            ("uart_parity", ConfigValue::Str("even")),
            ("uart_stop_bits", ConfigValue::Int(2)),
        ],
    )
    .unwrap();
    assert_eq!(
        wiring.uart.format(),
        Some(UartFormat {
            baud: 9600,
            parity: Parity::Even,
            stop_bits: StopBits::Two,
        })
    );

    let mut wiring = wired();
    bind(
        &mut wiring.board,
        &[("uart_baud", ConfigValue::Int(115_200))],
    )
    .unwrap();
    assert_eq!(wiring.uart.format(), Some(UartFormat::new(115_200)));
}

#[test]
fn bad_frame_settings_are_refused_at_bind() {
    let cases: [&[(&str, ConfigValue<'_>)]; 6] = [
        &[("uart_baud", ConfigValue::Int(0))],
        &[("uart_baud", ConfigValue::Int(-9600))],
        &[("uart_baud", ConfigValue::Str("9600"))],
        &[
            ("uart_baud", ConfigValue::Int(9600)),
            ("uart_parity", ConfigValue::Str("mark")),
        ],
        &[
            ("uart_baud", ConfigValue::Int(9600)),
            ("uart_stop_bits", ConfigValue::Int(3)),
        ],
        &[("uart_parity", ConfigValue::Str("even"))],
    ];
    for entries in cases {
        let mut wiring = wired();
        assert_eq!(
            bind(&mut wiring.board, entries).err(),
            Some(Status::INVALID_ARGUMENT),
            "{entries:?}"
        );
    }
    // A frame for a port the board did not give.
    let mut board = SimBoard::new();
    board.add_line("line");
    assert_eq!(
        bind(&mut board, &[("uart_baud", ConfigValue::Int(9600))]).err(),
        Some(Status::NOT_FOUND)
    );
}

#[test]
fn bytes_go_out_and_come_back_in_the_call() {
    let mut wiring = wired();
    let mut driver = bind(&mut wiring.board, &WRITES).unwrap();
    assert_eq!(client(&mut driver).uart_write(b"AT\r\n"), Ok(4));
    assert_eq!(wiring.uart.sent(), b"AT\r\n");

    wiring.uart.receive(b"OK\r\nmore");
    let mut into = [0u8; 4];
    assert_eq!(client(&mut driver).uart_read(4, 10, &mut into), Ok(4));
    assert_eq!(&into, b"OK\r\n");
    let mut into = [0u8; 16];
    assert_eq!(client(&mut driver).uart_read(16, 0, &mut into), Ok(4));
    assert_eq!(&into[..4], b"more");
    assert_eq!(client(&mut driver).uart_read(16, 0, &mut into), Ok(0));
    assert_eq!(Driver::health(&driver), Health::Ok);
}

#[test]
fn writing_needs_writes_and_reading_does_not() {
    let mut wiring = wired();
    let mut driver = bind(&mut wiring.board, &[]).unwrap();
    assert_eq!(
        client(&mut driver).uart_write(b"x"),
        Err(Status::NOT_PERMITTED)
    );
    assert!(wiring.uart.sent().is_empty());
    wiring.uart.receive(b"hi");
    let mut into = [0u8; 2];
    assert_eq!(client(&mut driver).uart_read(2, 0, &mut into), Ok(2));
}

#[test]
fn a_read_that_asks_for_nothing_too_much_or_too_long_is_refused() {
    let mut wiring = wired();
    let mut driver = bind(&mut wiring.board, &WRITES).unwrap();
    let mut into = [0u8; 4];
    let mut api = client(&mut driver);
    assert_eq!(
        api.uart_read(0, 0, &mut into),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        api.uart_read(5, 0, &mut into),
        Err(Status::BUFFER_TOO_SMALL)
    );
    assert_eq!(
        api.uart_read(4, UART_MAX_WAIT_MS + 1, &mut into),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(api.uart_read(4, UART_MAX_WAIT_MS, &mut into), Ok(0));
    assert_eq!(api.uart_write(&[]), Err(Status::INVALID_ARGUMENT));
}

#[test]
fn a_device_without_a_uart_answers_not_supported() {
    let mut board = SimBoard::new();
    board.add_line("line");
    let mut driver = bind(&mut board, &WRITES).unwrap();
    let mut into = [0u8; 4];
    assert_eq!(
        client(&mut driver).uart_read(4, 0, &mut into),
        Err(Status::NOT_SUPPORTED)
    );
    assert_eq!(
        client(&mut driver).uart_write(b"x"),
        Err(Status::NOT_SUPPORTED)
    );
}

#[test]
fn a_device_with_only_a_uart_binds() {
    let mut board = SimBoard::new();
    let uart = board.add_uart("uart");
    let mut driver = bind(&mut board, &WRITES).unwrap();
    assert_eq!(client(&mut driver).uart_write(b"z"), Ok(1));
    assert_eq!(uart.sent(), b"z");
}

#[test]
fn a_failed_call_marks_health_and_the_next_call_tries_again() {
    let mut wiring = wired();
    let mut driver = bind(&mut wiring.board, &WRITES).unwrap();
    all_buses_down(&wiring);
    assert_eq!(client(&mut driver).uart_write(b"x"), Err(Status::BUSY));
    assert!(matches!(Driver::health(&driver), Health::Failed(_)));
    let faults = wiring.board.faults("uart").unwrap();
    faults.clear();
    assert_eq!(client(&mut driver).uart_write(b"x"), Ok(1));
    assert_eq!(Driver::health(&driver), Health::Ok);
}
