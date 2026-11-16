//! Vectors for every frame of the flash link. The expected bytes were computed outside this crate
//! (a separate CRC-16/CCITT-FALSE implementation), so the encoder and the decoder are checked against
//! the same numbers a PC side would compute.

use lemnos_flash_wire::frame::{
    FrameError, Op, REQUEST_LEN, Request, ResetKind, STATUS_NOT_COMPARED, crc16_ccitt, payload_crc,
};

fn hex(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).unwrap())
        .collect()
}

#[test]
fn the_crc_matches_the_standard_check_value_and_the_big_endian_placement() {
    assert_eq!(crc16_ccitt(b"123456789"), 0x29B1);
    assert_eq!(payload_crc(b"123456789"), [0x29, 0xB1]);
    assert_eq!(payload_crc(b"ab"), [0x69, 0xF0]);
    assert_eq!(payload_crc(&[0u8; 512]), [0x16, 0x34]);
}

#[test]
fn a_read_request_encodes_to_its_vector_and_decodes_back() {
    let wire = hex("4c 4d 01 00 02 00 01 00 00 00 cf 90");
    let request = Request::read(1, 2);
    assert_eq!(request.encode().to_vec(), wire);
    assert_eq!(Request::decode(&wire), Ok(request));
    assert_eq!(wire.len(), REQUEST_LEN);
}

#[test]
fn a_plain_write_encodes_to_its_vector() {
    let wire = hex("4c 4d 02 00 01 00 04 00 00 00 75 40");
    let request = Request::write(4, 1, false);
    assert_eq!(request.encode().to_vec(), wire);
    assert_eq!(Request::decode(&wire), Ok(request));
    assert!(!request.compared());
}

#[test]
fn a_compared_write_sets_the_reserved_bit_and_encodes_to_its_vector() {
    let wire = hex("4c 4d 02 01 01 00 04 00 00 00 cd 21");
    let request = Request::write(4, 1, true);
    assert_eq!(request.encode().to_vec(), wire);
    assert_eq!(Request::decode(&wire), Ok(request));
    assert!(request.compared());
}

#[test]
fn an_end_carries_its_reset_kind_in_the_reserved_byte() {
    let normal = hex("4c 4d 03 00 01 00 00 00 00 00 f8 62");
    assert_eq!(Request::end(ResetKind::Normal).encode().to_vec(), normal);
    assert_eq!(
        Request::decode(&normal).unwrap().reset_kind(),
        ResetKind::Normal
    );

    let maskrom = hex("4c 4d 03 03 01 00 00 00 00 00 20 e0");
    assert_eq!(Request::end(ResetKind::Maskrom).encode().to_vec(), maskrom);
    assert_eq!(
        Request::decode(&maskrom).unwrap().reset_kind(),
        ResetKind::Maskrom
    );
}

#[test]
fn an_end_with_an_unlisted_reset_byte_is_a_normal_reset_as_before() {
    let wire = Request {
        op: Op::End,
        reserved: 7,
        count: 1,
        lba: 0,
    }
    .encode();
    assert_eq!(
        Request::decode(&wire).unwrap().reset_kind(),
        ResetKind::Normal
    );
}

#[test]
fn an_identify_request_encodes_to_its_vector() {
    let wire = hex("4c 4d 04 00 01 00 00 00 00 00 3f 7a");
    assert_eq!(Request::identify().encode().to_vec(), wire);
    assert_eq!(Request::decode(&wire), Ok(Request::identify()));
}

#[test]
fn a_maximum_read_encodes_its_lba_little_endian() {
    let wire = hex("4c 4d 01 00 80 00 03 02 01 00 20 c9");
    assert_eq!(Request::read(0x0001_0203, 128).encode().to_vec(), wire);
    assert_eq!(Request::decode(&wire), Ok(Request::read(0x0001_0203, 128)));
}

#[test]
fn a_flipped_bit_is_a_crc_refusal() {
    let mut wire = hex("4c 4d 01 00 02 00 01 00 00 00 cf 90");
    wire[7] ^= 0x10;
    assert_eq!(Request::decode(&wire), Err(FrameError::Crc));
}

#[test]
fn a_wrong_length_or_magic_is_not_a_request() {
    assert_eq!(Request::decode(&[0u8; 11]), Err(FrameError::NotRequest));
    assert_eq!(Request::decode(&[0u8; 13]), Err(FrameError::NotRequest));
    let mut wire = hex("4c 4d 03 00 01 00 00 00 00 00 f8 62");
    wire[0] = b'X';
    assert_eq!(Request::decode(&wire), Err(FrameError::NotRequest));
}

#[test]
fn a_count_outside_one_to_128_is_refused() {
    assert_eq!(
        Request::decode(&hex("4c 4d 01 00 00 00 00 00 00 00 32 64")),
        Err(FrameError::Count(0))
    );
    assert_eq!(
        Request::decode(&hex("4c 4d 01 00 81 00 00 00 00 00 a3 e4")),
        Err(FrameError::Count(129))
    );
}

#[test]
fn an_unknown_op_with_a_good_crc_is_refused_by_number() {
    assert_eq!(
        Request::decode(&hex("4c 4d 09 00 01 00 00 00 00 00 69 1e")),
        Err(FrameError::Op(9))
    );
}

#[test]
fn a_reserved_bit_the_op_does_not_define_is_refused() {
    assert_eq!(
        Request::decode(&hex("4c 4d 01 01 01 00 00 00 00 00 cf a5")),
        Err(FrameError::Reserved {
            op: Op::Read,
            reserved: 1
        })
    );
    assert_eq!(
        Request::decode(&hex("4c 4d 04 01 01 00 00 00 00 00 87 1b")),
        Err(FrameError::Reserved {
            op: Op::Identify,
            reserved: 1
        })
    );
    assert_eq!(
        Request::decode(&hex("4c 4d 02 02 01 00 00 00 00 00 df 52")),
        Err(FrameError::Reserved {
            op: Op::Write,
            reserved: 2
        })
    );
    assert_eq!(
        Request::decode(&hex("4c 4d 02 03 01 00 00 00 00 00 67 33")),
        Err(FrameError::Reserved {
            op: Op::Write,
            reserved: 3
        }),
        "a write takes the compared bit only"
    );
}

#[test]
fn the_status_for_a_refused_boot_area_write_is_four() {
    assert_eq!(STATUS_NOT_COMPARED, 4);
}
