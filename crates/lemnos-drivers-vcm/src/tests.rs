use super::*;
use lemnos_hal::mock::{MockDelay, MockI2c, MockOp, block_on};

extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;

fn encode(format: &VcmFormat<'_>, position: i32) -> Vec<u8> {
    let (bytes, n, _) = format.encode(position);
    bytes[..n].to_vec()
}

/// Styx's `lens::tests::chip_formats`.
#[test]
fn chip_formats() {
    // DW9714: 0x1ff << 4.
    assert_eq!(encode(&VcmChip::Dw9714.format(), 0x1ff), [0x1f, 0xf0]);
    assert_eq!(encode(&VcmChip::Dw9807.format(), 0x2a5), [0x03, 0x02, 0xa5]);
    assert_eq!(encode(&VcmChip::Dw9807.format(), 5000), [0x03, 0x03, 0xff]);
    assert_eq!(encode(&VcmChip::Dw9817.format(), -3), [0x03, 0x00, 0x00]);
    assert_eq!(encode(&VcmChip::Ak7375.format(), 0xabc), [0x00, 0xab, 0xc0]);
    assert_eq!(VcmChip::Ak7375.format().max_position(), 4095);
    let bad = VcmFormat {
        bytes: 1,
        bits: 10,
        ..VcmChip::Dw9807.format()
    };
    assert_eq!(bad.check(), Err(FormatError::Overflow));
    let bad = VcmFormat {
        bytes: 3,
        ..VcmChip::Dw9807.format()
    };
    assert_eq!(bad.check(), Err(FormatError::Width));
    for chip in [
        VcmChip::Dw9714,
        VcmChip::Dw9807,
        VcmChip::Dw9817,
        VcmChip::Ak7375,
    ] {
        chip.format().check().unwrap();
    }
    assert_eq!(VcmChip::from_name("dw9807-vcm"), Some(VcmChip::Dw9807));
    assert_eq!(VcmChip::from_name("dw9817 10-000c"), Some(VcmChip::Dw9817));
    assert_eq!(VcmChip::from_name("imx708"), None);
}

fn writes(i2c: &MockI2c) -> Vec<(u8, Vec<u8>)> {
    i2c.transfers()
        .iter()
        .map(|t| match &t.ops[..] {
            [MockOp::Write(bytes)] => (t.address, bytes.clone()),
            other => panic!("unexpected {other:?}"),
        })
        .collect()
}

#[test]
fn dw9817_powers_up_moves_and_stands_by() {
    let i2c = MockI2c::new().with_raw_target(DEFAULT_ADDRESS);
    let mut lens = Vcm::dw9817(i2c);
    let mut delay = MockDelay::new();
    lens.power_up(&mut delay).unwrap();
    assert!(lens.is_powered());
    assert_eq!(delay.total_ns, 1_000_000);
    assert_eq!(lens.move_to(445).unwrap(), 445);
    assert_eq!(lens.move_to(2000).unwrap(), 1023);
    assert_eq!(lens.position(), Some(1023));
    lens.power_down().unwrap();
    assert_eq!(
        writes(&lens.release()),
        [
            (0x0c, vec![0x02, 0x00]),
            (0x0c, vec![0x03, 0x01, 0xbd]),
            (0x0c, vec![0x03, 0x03, 0xff]),
            (0x0c, vec![0x02, 0x01]),
        ]
    );
}

#[test]
fn dw9714_and_ak7375_messages() {
    let mut lens = Vcm::dw9714(MockI2c::new().with_raw_target(0x0c));
    let mut delay = MockDelay::new();
    lens.power_up(&mut delay).unwrap();
    assert_eq!(delay.total_ns, 12_000_000);
    lens.move_to(0x1ff).unwrap();
    lens.power_down().unwrap();
    assert_eq!(
        writes(&lens.release()),
        [(0x0c, vec![0x1f, 0xf0]), (0x0c, vec![0x80, 0x00])]
    );

    let mut lens = Vcm::ak7375(MockI2c::new().with_raw_target(0x0c));
    lens.power_up(&mut MockDelay::new()).unwrap();
    lens.move_to(0xabc).unwrap();
    assert_eq!(
        writes(&lens.release()),
        [(0x0c, vec![0x02, 0x00]), (0x0c, vec![0x00, 0xab, 0xc0])]
    );
}

#[test]
fn custom_formats_and_errors() {
    const UP: &[&[u8]] = &[&[0xec, 0xa3], &[0xa1, 0x05]];
    let format = VcmFormat {
        register: None,
        bytes: 2,
        shift: 4,
        bits: 10,
        or: 0x000f,
        power_up: UP,
        power_up_us: 0,
        power_down: &[],
    };
    let mut lens = Vcm::new(MockI2c::new().with_raw_target(0x0d), 0x0d, format).unwrap();
    lens.power_up(&mut MockDelay::new()).unwrap();
    lens.move_to(1).unwrap();
    assert_eq!(
        writes(&lens.release()),
        [
            (0x0d, vec![0xec, 0xa3]),
            (0x0d, vec![0xa1, 0x05]),
            (0x0d, vec![0x00, 0x1f])
        ]
    );
    let bad = VcmFormat { bits: 0, ..format };
    assert!(Vcm::new(MockI2c::new(), 0x0d, bad).is_err());
    // Nobody at the address: a NACK.
    let mut lens = Vcm::dw9807(MockI2c::new());
    let err = lens.move_to(1).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Nack);
}

#[test]
fn async_driver_matches_blocking() {
    let mut blocking = Vcm::dw9807(MockI2c::new().with_raw_target(0x0c));
    blocking.power_up(&mut MockDelay::new()).unwrap();
    blocking.move_to(700).unwrap();
    blocking.power_down().unwrap();

    let mut lens = asynch::Vcm::chip(MockI2c::new().with_raw_target(0x0c), VcmChip::Dw9807);
    let mut delay = MockDelay::new();
    block_on(async {
        lens.power_up(&mut delay).await.unwrap();
        lens.move_to(700).await.unwrap();
        lens.power_down().await.unwrap();
    });
    assert_eq!(lens.position(), Some(700));
    assert!(!lens.is_powered());
    assert_eq!(writes(&blocking.release()), writes(&lens.release()));
}

#[test]
fn device_model_position_control() {
    use lemnos_device::{DeviceRef, Quantity};
    let mut lens = Vcm::dw9817(MockI2c::new().with_raw_target(DEFAULT_ADDRESS));
    let mut device = DeviceRef::control(&mut lens);
    assert!(!device.is_sensor());
    let info = device.info();
    assert_eq!(info.controls[0].quantity, Quantity::Position);
    assert_eq!(info.controls[0].max, 1023);
    device.init(&mut MockDelay::new()).unwrap();
    assert_eq!(device.get(0), Err(lemnos_hal::ErrorKind::Unsupported));
    assert_eq!(device.set(0, 445), Ok(445));
    assert_eq!(device.get(0), Ok(445));
    assert_eq!(
        device.set(0, 1024),
        Err(lemnos_hal::ErrorKind::InvalidInput)
    );
    assert_eq!(
        device.read(&mut [0]),
        Err(lemnos_hal::ErrorKind::Unsupported)
    );
    assert_eq!(info_for_bits(12).controls[0].max, 4095);

    use lemnos_device::asynch::{Control, Device};
    let mut lens = asynch::Vcm::chip(MockI2c::new().with_raw_target(0x0c), VcmChip::Ak7375);
    block_on(Device::init(&mut lens, &mut MockDelay::new())).unwrap();
    assert_eq!(block_on(Control::set(&mut lens, 0, 4095)), Ok(4095));
    assert_eq!(block_on(Control::get(&mut lens, 0)), Ok(4095));
}

#[test]
fn custom_chip_has_no_builtin_format() {
    assert_eq!(VcmChip::Custom.builtin_format(), None);
    assert_eq!(VcmChip::from_name("custom"), Some(VcmChip::Custom));
    assert_eq!(VcmChip::Custom.format().check(), Err(FormatError::Unset));
    assert!(matches!(
        Vcm::new(MockI2c::new(), 0x0c, VcmChip::Custom.format()),
        Err(FormatError::Unset)
    ));
    for chip in VcmChip::BUILTIN {
        assert_eq!(VcmChip::from_name(chip.name()), Some(chip));
        assert_eq!(chip.builtin_format(), Some(chip.format()));
    }
}

#[cfg(feature = "alloc")]
#[test]
fn owned_formats_lend_a_format() {
    let owned = OwnedVcmFormat::for_chip(VcmChip::Dw9807).unwrap();
    assert_eq!(OwnedVcmFormat::for_chip(VcmChip::Custom), None);
    owned.check().unwrap();
    assert_eq!(owned.max_position(), 1023);
    let refs = owned.refs();
    assert_eq!(refs.format(), VcmChip::Dw9807.format());
    let i2c = MockI2c::new().with_raw_target(0x0c);
    let mut lens = Vcm::new(i2c.clone(), 0x0c, refs.format()).unwrap();
    lens.power_up(&mut MockDelay::new()).unwrap();
    assert_eq!(lens.move_to(0x2a5).unwrap(), 0x2a5);
    assert_eq!(
        writes(&i2c),
        [(0x0c, vec![0x02, 0x00]), (0x0c, vec![0x03, 0x02, 0xa5])]
    );

    let too_many = OwnedVcmFormat {
        power_up: vec![vec![0]; MAX_VCM_WRITES + 1],
        ..OwnedVcmFormat::default()
    };
    assert_eq!(too_many.check(), Err(FormatError::TooManyWrites));
}

#[cfg(feature = "serde")]
#[test]
fn chips_serialize_by_name() {
    assert_eq!(
        serde_json::to_string(&VcmChip::Dw9817).unwrap(),
        "\"dw9817\""
    );
    let chip: VcmChip = serde_json::from_str("\"custom\"").unwrap();
    assert_eq!(chip, VcmChip::Custom);
    assert!(serde_json::from_str::<VcmChip>("\"imx708\"").is_err());
}

#[cfg(all(feature = "serde", feature = "alloc"))]
#[test]
fn owned_formats_deserialize_with_defaults() {
    let format: OwnedVcmFormat =
        serde_json::from_str(r#"{"register": 3, "power_up": [[2, 0]], "power_down": [[2, 1]]}"#)
            .unwrap();
    assert_eq!(format.bytes, 2);
    assert_eq!(format.bits, 10);
    assert_eq!(
        format.refs().format().power_up,
        VcmChip::Dw9807.format().power_up
    );
    let json = serde_json::to_string(&format).unwrap();
    assert_eq!(
        serde_json::from_str::<OwnedVcmFormat>(&json).unwrap(),
        format
    );
    assert!(serde_json::from_str::<OwnedVcmFormat>(r#"{"width": 2}"#).is_err());
}
