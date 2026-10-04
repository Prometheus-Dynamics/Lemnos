use super::*;
use crate::mock::{MockI2c, MockOp, MockSpi, block_on};
use crate::{ErrorKind, HalError};
use core::convert::Infallible;

extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;

const OV9782: u8 = 0x60;

fn ov9782() -> I2cRegisters<MockI2c> {
    let i2c = MockI2c::new()
        .with_target(OV9782, AddressWidth::Bits16)
        .with_registers(OV9782, 0x300a, &[0x97, 0x82]);
    I2cRegisters::new(i2c, OV9782, AddressWidth::Bits16)
}

fn writes_of(bus: &I2cRegisters<MockI2c>) -> Vec<Vec<u8>> {
    bus.i2c()
        .transfers()
        .iter()
        .map(|t| {
            assert_eq!(t.ops.len(), 1, "one message per transfer");
            match &t.ops[0] {
                MockOp::Write(bytes) => bytes.clone(),
                other => panic!("unexpected {other:?}"),
            }
        })
        .collect()
}

#[test]
fn encodes_register_writes() {
    let e = |a, b, v, w| encode_write::<Infallible>(a, b, v, w, Endian::Big);
    let (b, n) = e(0x3008, 1, 0x82, AddressWidth::Bits16).unwrap();
    assert_eq!(&b[..n], &[0x30, 0x08, 0x82]);
    let (b, n) = e(0x380c, 2, 0x05b0, AddressWidth::Bits16).unwrap();
    assert_eq!(&b[..n], &[0x38, 0x0c, 0x05, 0xb0]);
    let (b, n) = e(0x3500, 3, 0x002820, AddressWidth::Bits16).unwrap();
    assert_eq!(&b[..n], &[0x35, 0x00, 0x00, 0x28, 0x20]);
    let (b, n) = e(0x10, 4, 0x1234_5678, AddressWidth::Bits8).unwrap();
    assert_eq!(&b[..n], &[0x10, 0x12, 0x34, 0x56, 0x78]);
    let (b, n) =
        encode_write::<Infallible>(0x10, 2, 0x1234, AddressWidth::Bits8, Endian::Little).unwrap();
    assert_eq!(&b[..n], &[0x10, 0x34, 0x12]);
    assert_eq!(
        e(0x100, 1, 0, AddressWidth::Bits8),
        Err(RegisterError::AddressTooWide(0x100))
    );
    assert!(matches!(
        e(0x10, 1, 0x100, AddressWidth::Bits8),
        Err(RegisterError::ValueTooWide { .. })
    ));
    assert_eq!(
        e(0x10, 0, 0, AddressWidth::Bits8),
        Err(RegisterError::InvalidWidth(0))
    );
    assert_eq!(
        e(0x10, 5, 0, AddressWidth::Bits8),
        Err(RegisterError::InvalidWidth(5))
    );
}

#[test]
fn decodes_values() {
    assert_eq!(decode_value(&[0x97], Endian::Big), 0x97);
    assert_eq!(decode_value(&[0x97, 0x82], Endian::Big), 0x9782);
    assert_eq!(decode_value(&[1, 2, 3, 4], Endian::Big), 0x0102_0304);
    assert_eq!(decode_value(&[0x34, 0x12], Endian::Little), 0x1234);
}

#[test]
fn two_byte_chip_id_is_one_combined_read_with_a_16_bit_address() {
    let mut bus = ov9782();
    assert_eq!(bus.read(0x300a, 2).unwrap(), 0x9782);
    assert_eq!(bus.read16(0x300a).unwrap(), 0x9782);
    let t = &bus.i2c().transfers()[0];
    assert_eq!(t.address, OV9782);
    assert_eq!(t.ops, [MockOp::Write(vec![0x30, 0x0a]), MockOp::Read(2)]);
}

#[test]
fn sequences_are_one_transfer_per_write() {
    let mut bus = ov9782();
    let writes: Vec<RegWrite> = (0..50)
        .map(|i| RegWrite::byte(0x3000 + i, i as u8))
        .collect();
    bus.write_sequence(&writes).unwrap();
    let t = writes_of(&bus);
    assert_eq!(t.len(), 50);
    assert_eq!(t[1], [0x30, 0x01, 0x01]);
}

#[test]
fn bursts_join_consecutive_registers_in_order() {
    let mut bus = ov9782().with_bursts(32);
    let w = RegWrite::byte;
    let writes = [
        w(0x3800, 1),
        w(0x3801, 2),
        RegWrite::new(0x3802, 2, 0x0304),
        w(0x3805, 5), // a gap: new transfer
        w(0x3806, 6),
        w(0x3805, 7), // going back: new transfer, written after
    ];
    bus.write_sequence(&writes).unwrap();
    let t = writes_of(&bus);
    assert_eq!(t.len(), 3);
    assert_eq!(t[0], [0x38, 0x00, 1, 2, 3, 4]);
    assert_eq!(t[1], [0x38, 0x05, 5, 6]);
    assert_eq!(bus.i2c().register(OV9782, 0x3805), 7);
    // Long runs are cut at the burst length.
    let mut bus = ov9782().with_bursts(32);
    let run: Vec<RegWrite> = (0..70).map(|i| w(0x5000 + i, i as u8)).collect();
    bus.write_sequence(&run).unwrap();
    let lens: Vec<usize> = writes_of(&bus).iter().map(|m| m.len() - 2).collect();
    assert_eq!(lens, [32, 32, 6]);
}

#[test]
fn long_bursts_are_one_contiguous_write() {
    let mut bus = ov9782();
    let data: Vec<u8> = (0..200).map(|i| i as u8).collect();
    bus.write_burst(0x6000, &data).unwrap();
    let t = &bus.i2c().transfers()[0];
    assert_eq!(t.ops.len(), 2);
    let run = &bus.i2c().target(OV9782).unwrap().raw_writes[0];
    assert_eq!(run.len(), 202);
    assert_eq!(bus.i2c().register(OV9782, 0x6000 + 199), 199);
    let mut back = [0u8; 200];
    bus.read_burst(0x6000, &mut back).unwrap();
    assert_eq!(&back[..], &data[..]);
}

#[test]
fn modify_and_verify() {
    let mut bus = ov9782();
    bus.write8(0x0100, 0b1010_0000).unwrap();
    assert_eq!(bus.modify(0x0100, 1, 0x0f, 0x05).unwrap(), 0b1010_0101);
    bus.write_verified(0x0101, 2, 0xbeef).unwrap();
    assert_eq!(bus.read16(0x0101).unwrap(), 0xbeef);
    let mut little = ov9782().with_endian(Endian::Little);
    little.write16(0x10, 0x1234).unwrap();
    assert_eq!(little.i2c().register(OV9782, 0x10), 0x34);
    assert_eq!(little.read16(0x10).unwrap(), 0x1234);
}

#[test]
fn bus_errors_keep_their_kind() {
    let mut missing = I2cRegisters::new(MockI2c::new(), 0x10, AddressWidth::Bits8);
    let err = missing.read8(0).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Nack);
    assert_eq!(err.bus_error(), Some(&ErrorKind::Nack));
    let mut bus = ov9782();
    // Kinds travel through embedded-hal's I2C error kinds: busy (arbitration
    // loss) survives, a timeout becomes a generic failure.
    bus.i2c_mut().fail_next(ErrorKind::Busy);
    assert_eq!(bus.write8(0x0100, 1).unwrap_err().kind(), ErrorKind::Busy);
    bus.i2c_mut().fail_next(ErrorKind::Timeout);
    let err = bus.write8(0x0100, 1).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Failed);
    assert_eq!(err.bus_error(), Some(&ErrorKind::Timeout));
    assert_eq!(
        bus.read(0x0100, 5).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
}

#[test]
fn async_bus_matches_blocking() {
    let writes: Vec<RegWrite> = (0..40)
        .map(|i| RegWrite::byte(0x5000 + i, i as u8))
        .collect();
    let mut blocking = ov9782().with_bursts(16);
    blocking.write_sequence(&writes).unwrap();
    let mut asynchronous = ov9782().with_bursts(16);
    block_on(asynch::RegisterBus::write_sequence(
        &mut asynchronous,
        &writes,
    ))
    .unwrap();
    assert_eq!(writes_of(&blocking), writes_of(&asynchronous));
    let id = block_on(asynch::RegisterBus::read(&mut asynchronous, 0x300a, 2)).unwrap();
    assert_eq!(id, 0x9782);
    let new = block_on(asynch::RegisterBus::modify(
        &mut asynchronous,
        0x5001,
        1,
        0xf0,
        0xa0,
    ))
    .unwrap();
    assert_eq!(new, 0xa1);
}

#[test]
fn spi_registers_flag_reads_and_multi_byte_transfers() {
    let spi = MockSpi::new().with_response(&[0x1e, 0x34, 0x12]);
    let mut regs = SpiRegisters::new(spi, AddressWidth::Bits8)
        .with_multi_byte_flag(0x40)
        .with_endian(Endian::Little);
    assert_eq!(regs.read8(0x00).unwrap(), 0x1e);
    assert_eq!(regs.read16(0x12).unwrap(), 0x1234);
    regs.write8(0x7e, 0xb6).unwrap();
    let t = &regs.spi().transactions;
    assert_eq!(t[0], [MockOp::Write(vec![0x80]), MockOp::Read(1)]);
    assert_eq!(t[1], [MockOp::Write(vec![0xd2]), MockOp::Read(2)]);
    assert_eq!(t[2], [MockOp::Write(vec![0x7e]), MockOp::Write(vec![0xb6])]);
    assert_eq!(
        regs.read8(0x80).unwrap_err(),
        RegisterError::AddressTooWide(0x80)
    );
}

#[test]
fn spi_bursts_and_async() {
    let mut regs = SpiRegisters::new(MockSpi::new(), AddressWidth::Bits8).with_bursts(8);
    let writes = [
        RegWrite::byte(0x10, 1),
        RegWrite::byte(0x11, 2),
        RegWrite::byte(0x20, 3),
    ];
    regs.write_sequence(&writes).unwrap();
    assert_eq!(
        regs.spi().transactions,
        [
            vec![MockOp::Write(vec![0x10, 1, 2])],
            vec![MockOp::Write(vec![0x20, 3])]
        ]
    );
    let mut regs = SpiRegisters::new(MockSpi::new().with_response(&[7]), AddressWidth::Bits8);
    assert_eq!(
        block_on(asynch::RegisterBus::read8(&mut regs, 0x01)).unwrap(),
        7
    );
    block_on(asynch::RegisterBus::write_sequence(&mut regs, &writes)).unwrap();
    assert_eq!(regs.spi().transactions.len(), 4);
}
