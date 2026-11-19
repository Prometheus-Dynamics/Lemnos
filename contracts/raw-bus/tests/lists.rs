//! The encoded transfer lists: what an encoder writes, a parser takes back, and what it refuses.

use lemnos_contract_raw_bus::{
    I2cOp, I2cOps, MAX_I2C_OPS, MAX_SPI_TRANSFERS, SpiTransfer, SpiTransfers, encode_i2c_ops,
    encode_spi_transfers,
};
use lemnos_core::Status;

fn i2c_bytes(ops: &[I2cOp<'_>]) -> Vec<u8> {
    let mut out = vec![0u8; 4096];
    let n = encode_i2c_ops(ops, &mut out).unwrap();
    out.truncate(n);
    out
}

fn spi_bytes(transfers: &[SpiTransfer<'_>]) -> Vec<u8> {
    let mut out = vec![0u8; 4096];
    let n = encode_spi_transfers(transfers, &mut out).unwrap();
    out.truncate(n);
    out
}

#[test]
fn an_i2c_list_comes_back_as_it_went_in() {
    let ops = [
        I2cOp::Write(&[0x10]),
        I2cOp::Read(2),
        I2cOp::Write(&[1, 2, 3]),
    ];
    let bytes = i2c_bytes(&ops);
    let parsed = I2cOps::parse(&bytes, 2).unwrap();
    assert_eq!(parsed.len(), 3);
    assert_eq!(parsed.read_total(), 2);
    assert_eq!(parsed.iter().collect::<Vec<_>>(), ops);
}

#[test]
fn an_i2c_list_says_whether_it_needs_writes() {
    let read_only = i2c_bytes(&[I2cOp::Write(&[0x10]), I2cOp::Read(1)]);
    assert!(!I2cOps::parse(&read_only, 1).unwrap().needs_writes());
    let write_after_read = i2c_bytes(&[I2cOp::Read(1), I2cOp::Write(&[9])]);
    assert!(I2cOps::parse(&write_after_read, 1).unwrap().needs_writes());
    let write_only = i2c_bytes(&[I2cOp::Write(&[9])]);
    assert!(I2cOps::parse(&write_only, 0).unwrap().needs_writes());
}

#[test]
fn an_i2c_list_past_its_bound_is_refused_by_name() {
    let ops = [I2cOp::Read(1); MAX_I2C_OPS + 1];
    let bytes = i2c_bytes(&ops);
    assert_eq!(
        I2cOps::parse(&bytes, 99).unwrap_err(),
        Status::INVALID_ARGUMENT
    );
    let full = i2c_bytes(&[I2cOp::Read(1); MAX_I2C_OPS]);
    assert_eq!(
        I2cOps::parse(&full, MAX_I2C_OPS).unwrap().len(),
        MAX_I2C_OPS
    );
}

#[test]
fn a_malformed_i2c_list_is_refused() {
    assert_eq!(I2cOps::parse(&[], 0).unwrap_err(), Status::INVALID_ARGUMENT);
    // An empty write, a zero-length read, a kind that is neither, a truncated header and payload.
    for bad in [
        vec![0, 0, 0, 0, 0],
        vec![1, 0, 0, 0, 0],
        vec![7, 1, 0, 0, 0],
        vec![0, 1, 0],
        vec![0, 4, 0, 0, 0, 1],
    ] {
        assert_eq!(
            I2cOps::parse(&bad, 99).unwrap_err(),
            Status::INVALID_ARGUMENT,
            "{bad:?}"
        );
    }
    // Reads past the reply buffer.
    let reads = i2c_bytes(&[I2cOp::Read(5)]);
    assert_eq!(
        I2cOps::parse(&reads, 4).unwrap_err(),
        Status::INVALID_ARGUMENT
    );
}

#[test]
fn an_spi_list_comes_back_with_each_transfers_own_settings() {
    let transfers = [
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
    ];
    let bytes = spi_bytes(&transfers);
    let parsed = SpiTransfers::parse(&bytes, 3).unwrap();
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed.read_total(), 3);
    assert!(!parsed.needs_writes());
    assert_eq!(parsed.iter().collect::<Vec<_>>(), transfers);
}

#[test]
fn an_spi_list_refuses_what_the_device_cannot_run() {
    let base = SpiTransfer {
        mode: 0,
        speed_hz: 0,
        bits_per_word: 0,
        cs_change: false,
        delay_us: 0,
        write: &[1],
        read: 1,
    };
    let refused = |transfer: SpiTransfer<'_>| {
        let bytes = spi_bytes(&[transfer]);
        SpiTransfers::parse(&bytes, 99).unwrap_err()
    };
    assert_eq!(
        refused(SpiTransfer { mode: 4, ..base }),
        Status::INVALID_ARGUMENT
    );
    assert_eq!(
        refused(SpiTransfer {
            bits_per_word: 7,
            ..base
        }),
        Status::INVALID_ARGUMENT
    );
    assert_eq!(
        refused(SpiTransfer {
            bits_per_word: 33,
            ..base
        }),
        Status::INVALID_ARGUMENT
    );
    assert_eq!(
        refused(SpiTransfer {
            write: &[],
            read: 0,
            ..base
        }),
        Status::INVALID_ARGUMENT,
        "a transfer with nothing to do"
    );
    let many = vec![base; MAX_SPI_TRANSFERS + 1];
    let bytes = spi_bytes(&many);
    assert_eq!(
        SpiTransfers::parse(&bytes, 99).unwrap_err(),
        Status::INVALID_ARGUMENT
    );
}

#[test]
fn an_spi_transfer_with_no_read_part_needs_writes() {
    let bytes = spi_bytes(&[SpiTransfer {
        mode: 0,
        speed_hz: 0,
        bits_per_word: 0,
        cs_change: false,
        delay_us: 0,
        write: &[1],
        read: 0,
    }]);
    assert!(SpiTransfers::parse(&bytes, 0).unwrap().needs_writes());
}

#[test]
fn an_encoder_refuses_a_buffer_too_small_for_the_list() {
    let mut out = [0u8; 3];
    assert_eq!(
        encode_i2c_ops(&[I2cOp::Write(&[1])], &mut out).unwrap_err(),
        Status::BUFFER_TOO_SMALL
    );
}
