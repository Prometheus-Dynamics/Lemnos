//! The transfer lists on the simulated board, and the SPI list's settings on a recording SPI device,
//! which the sim does not keep (its `MockRawSpi` records what the sim models, not chip select or delays).

mod common;

use std::sync::{Arc, Mutex};

use common::{all_buses_down, client, wired};
use lemnos_contract_raw_bus::{
    I2cOp, MAX_I2C_OPS, MAX_SPI_TRANSFERS, RawBus as _, SpiTransfer, encode_i2c_ops,
    encode_spi_transfers,
};
use lemnos_core::{
    Config, ConfigValue, Driver, Held, Lifecycle, MethodId, RawSpiBus, Reply, Status,
};
use lemnos_driver_raw_bus::{RawBus, RawBusNeeds};
use lemnos_hal::ErrorKind;
use lemnos_hal::raw::{RawSpi, SpiMode, SpiSegment};

const WRITES: [(&str, ConfigValue<'static>); 2] = [
    ("writes", ConfigValue::Bool(true)),
    ("allow_i2c", ConfigValue::Int(0x50)),
];
const READS_ONLY: [(&str, ConfigValue<'static>); 1] = [("allow_i2c", ConfigValue::Int(0x50))];

fn i2c_list(ops: &[I2cOp<'_>]) -> Vec<u8> {
    let mut out = vec![0u8; 1024];
    let n = encode_i2c_ops(ops, &mut out).unwrap();
    out.truncate(n);
    out
}

fn spi_list(transfers: &[SpiTransfer<'_>]) -> Vec<u8> {
    let mut out = vec![0u8; 1024];
    let n = encode_spi_transfers(transfers, &mut out).unwrap();
    out.truncate(n);
    out
}

#[test]
fn an_i2c_list_writes_a_pointer_and_reads_back_in_one_transaction() {
    let mut wiring = wired();
    let mut driver = common::bind(&mut wiring.board, &READS_ONLY).unwrap();
    let ops = i2c_list(&[I2cOp::Write(&[0x10]), I2cOp::Read(2)]);
    let mut into = [0u8; 4];
    assert_eq!(
        client(&mut driver).i2c_transactions(0x50, &ops, &mut into),
        Ok(2)
    );
    assert_eq!(&into[..2], [0xaa, 0xbb]);
}

#[test]
fn an_i2c_list_that_writes_after_a_read_needs_writes_on_the_device() {
    let mut wiring = wired();
    let ops = i2c_list(&[I2cOp::Read(1), I2cOp::Write(&[0x20, 0x42])]);
    {
        let mut driver = common::bind(&mut wiring.board, &READS_ONLY).unwrap();
        assert_eq!(
            client(&mut driver).i2c_transactions(0x50, &ops, &mut [0u8; 1]),
            Err(Status::NOT_PERMITTED)
        );
    }
    assert_eq!(
        wiring.i2c.byte(0x50, 0x20),
        None,
        "refused: nothing reached the bus"
    );
    let mut driver = common::bind(&mut wiring.board, &WRITES).unwrap();
    assert_eq!(
        client(&mut driver).i2c_transactions(0x50, &ops, &mut [0u8; 1]),
        Ok(1)
    );
    assert_eq!(wiring.i2c.byte(0x50, 0x20), Some(0x42));
}

/// A list with no read part is a write, so with `writes` off it is refused. Every bus is down, so a
/// call that got past the policy would return `BUSY` instead of `NOT_PERMITTED`.
#[test]
fn an_i2c_list_with_no_read_part_is_refused_with_writes_off_and_reaches_no_bus() {
    let mut wiring = wired();
    let mut driver = common::bind(&mut wiring.board, &READS_ONLY).unwrap();
    all_buses_down(&wiring);
    let before = wiring.i2c.transactions();
    let ops = i2c_list(&[I2cOp::Write(&[0x20, 0x42])]);
    assert_eq!(
        client(&mut driver).i2c_transactions(0x50, &ops, &mut []),
        Err(Status::NOT_PERMITTED),
        "a list with no read part is a write"
    );
    assert_eq!(
        wiring.i2c.transactions(),
        before,
        "refused: no bus was reached"
    );
}

#[test]
fn an_i2c_list_past_its_bound_or_its_reply_is_refused_by_name() {
    let mut wiring = wired();
    let mut driver = common::bind(&mut wiring.board, &WRITES).unwrap();
    let too_many = i2c_list(&[I2cOp::Read(1); MAX_I2C_OPS + 1]);
    assert_eq!(
        client(&mut driver).i2c_transactions(0x50, &too_many, &mut [0u8; 9]),
        Err(Status::INVALID_ARGUMENT)
    );
    let two = i2c_list(&[I2cOp::Read(2)]);
    assert_eq!(
        client(&mut driver).i2c_transactions(0x50, &two, &mut [0u8; 1]),
        Err(Status::INVALID_ARGUMENT),
        "a reply that cannot hold the reads"
    );
    assert_eq!(
        client(&mut driver).i2c_transactions(0x50, &[], &mut [0u8; 1]),
        Err(Status::INVALID_ARGUMENT),
        "an empty list"
    );
}

/// An SPI device that keeps every segment it was given, and answers each read with counting bytes.
#[derive(Debug, Default)]
struct Recorder {
    segments: Arc<Mutex<Vec<Seen>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    mode: SpiMode,
    speed_hz: u32,
    bits_per_word: u8,
    cs_change: bool,
    delay_us: u16,
    tx: Vec<u8>,
    rx_len: usize,
}

impl RawSpi for Recorder {
    fn transfer(&mut self, segments: &mut [SpiSegment<'_>]) -> Result<(), ErrorKind> {
        let mut seen = self.segments.lock().unwrap();
        let mut next = 0x40u8;
        for segment in segments.iter_mut() {
            seen.push(Seen {
                mode: segment.config.mode,
                speed_hz: segment.config.speed_hz,
                bits_per_word: segment.config.bits_per_word,
                cs_change: segment.cs_change,
                delay_us: segment.delay_us,
                tx: segment.tx.to_vec(),
                rx_len: segment.rx.len(),
            });
            for byte in segment.rx.iter_mut() {
                *byte = next;
                next += 1;
            }
        }
        Ok(())
    }
}

/// A driver whose SPI list is the recorder, with no other bus.
fn spi_driver(recorder: &Recorder, config: &Config<'_>) -> RawBus {
    let raw: RawSpiBus = Held::boxed(Box::new(Recorder {
        segments: Arc::clone(&recorder.segments),
    }));
    let needs = RawBusNeeds {
        i2c: None,
        spi: None,
        raw_spi: Some(raw),
        line: None,
        pwm: None,
        uart: None,
    };
    <RawBus as Lifecycle>::start(needs, config).unwrap()
}

#[test]
fn an_spi_list_gives_each_transfer_its_own_settings_chip_select_and_delay() {
    let recorder = Recorder::default();
    let entries = [("writes", ConfigValue::Bool(false))];
    let mut driver = spi_driver(&recorder, &Config::new(&entries));
    let list = spi_list(&[
        SpiTransfer {
            mode: 3,
            speed_hz: 1_000_000,
            bits_per_word: 8,
            cs_change: true,
            delay_us: 5,
            write: &[1, 2],
            read: 2,
        },
        SpiTransfer {
            mode: 0,
            speed_hz: 500_000,
            bits_per_word: 0,
            cs_change: false,
            delay_us: 0,
            write: &[],
            read: 1,
        },
    ]);
    let mut into = [0u8; 4];
    assert_eq!(
        client(&mut driver).spi_transfers(&list, &mut into),
        Ok(3),
        "the reads' total"
    );
    assert_eq!(into[..3], [0x40, 0x41, 0x42]);
    let seen = recorder.segments.lock().unwrap().clone();
    assert_eq!(
        seen,
        [
            Seen {
                mode: SpiMode::Mode3,
                speed_hz: 1_000_000,
                bits_per_word: 8,
                cs_change: true,
                delay_us: 5,
                tx: vec![1, 2],
                rx_len: 2,
            },
            Seen {
                mode: SpiMode::Mode0,
                speed_hz: 500_000,
                bits_per_word: 0,
                cs_change: false,
                delay_us: 0,
                tx: vec![],
                rx_len: 1,
            },
        ]
    );
}

#[test]
fn an_spi_list_with_a_write_needs_writes_and_a_bad_setting_is_refused() {
    let recorder = Recorder::default();
    let entries = [("writes", ConfigValue::Bool(false))];
    let mut driver = spi_driver(&recorder, &Config::new(&entries));
    let write_only = spi_list(&[SpiTransfer {
        mode: 0,
        speed_hz: 0,
        bits_per_word: 0,
        cs_change: false,
        delay_us: 0,
        write: &[9],
        read: 0,
    }]);
    assert_eq!(
        client(&mut driver).spi_transfers(&write_only, &mut []),
        Err(Status::NOT_PERMITTED)
    );
    let bad_mode = spi_list(&[SpiTransfer {
        mode: 4,
        speed_hz: 0,
        bits_per_word: 0,
        cs_change: false,
        delay_us: 0,
        write: &[1],
        read: 1,
    }]);
    assert_eq!(
        client(&mut driver).spi_transfers(&bad_mode, &mut [0u8; 1]),
        Err(Status::INVALID_ARGUMENT)
    );
    let many = spi_list(&vec![
        SpiTransfer {
            mode: 0,
            speed_hz: 0,
            bits_per_word: 0,
            cs_change: false,
            delay_us: 0,
            write: &[],
            read: 1,
        };
        MAX_SPI_TRANSFERS + 1
    ]);
    // Called with the driver's own dispatch: a local caller's arguments are capped at LOCAL_ARGS, and
    // this list is larger, so the driver's bound is what is under test.
    let id = MethodId {
        contract: 0,
        method: 9,
    };
    let mut reply = [0u8; 9];
    let status = driver.dispatch(id, &many, &mut Reply::new(&mut reply));
    assert_eq!(status, Status::INVALID_ARGUMENT);
    assert!(
        recorder.segments.lock().unwrap().is_empty(),
        "nothing reached the device"
    );
}

#[test]
fn a_device_without_raw_spi_says_the_list_is_not_supported() {
    let mut wiring = wired();
    let mut driver = common::bind(&mut wiring.board, &WRITES).unwrap();
    let list = spi_list(&[SpiTransfer {
        mode: 0,
        speed_hz: 0,
        bits_per_word: 0,
        cs_change: false,
        delay_us: 0,
        write: &[],
        read: 1,
    }]);
    assert_eq!(
        client(&mut driver).spi_transfers(&list, &mut [0u8; 1]),
        Err(Status::NOT_SUPPORTED)
    );
}
