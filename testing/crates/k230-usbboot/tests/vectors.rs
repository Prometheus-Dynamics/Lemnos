//! Command packets as bytes, derived by hand from k230_flash 33a75f0 `burner_uboot.cpp` ("U:n" is a
//! line of it). Nothing was captured from a board.

use lemnos_k230_usbboot::packet::{cmd, packet, words};

fn hex(text: &str) -> Vec<u8> {
    let text: String = text.split_whitespace().collect();
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}

/// A 60-byte packet written out by hand, zero padded.
fn pad(head: &str) -> Vec<u8> {
    let mut bytes = hex(head);
    bytes.resize(60, 0);
    bytes
}

#[test]
fn command_packets_match_the_c_layout() {
    // U:46-58: 60 bytes, header 6, data 54.
    assert_eq!(packet(cmd::NOP, &[]).to_vec(), pad("0000 0000 0000"));
    // U:503-515: DEV_PROBE data [medium, 0xFF]. SD is 2 (K:41), SPI_NAND 3.
    assert_eq!(
        packet(cmd::PROBE, &[2, 0xff]).to_vec(),
        pad("1000 0000 0200 02ff")
    );
    assert_eq!(
        packet(cmd::PROBE, &[3, 0xff]).to_vec(),
        pad("1000 0000 0200 03ff")
    );
    // U:545-550: DEV_GET_INFO, no data.
    assert_eq!(packet(cmd::GET_INFO, &[]).to_vec(), pad("1100 0000 0000"));
    // U:570-600: ERASE_LBA {offset, size}, data_size 16.
    assert_eq!(
        packet(cmd::ERASE, &words(&[0x10_0000, 0x4_0000])).to_vec(),
        pad("2000 0000 1000 0000100000000000 0000040000000000")
    );
    // U:633-690: WRITE_LBA with loader version >= 1: {offset, size, max, flag}.
    assert_eq!(
        packet(cmd::WRITE, &words(&[0x10_0000, 0x600, 0x2000, 0])).to_vec(),
        pad("2100 0000 2000 0000100000000000 0006000000000000 0020000000000000 0000000000000000")
    );
    // Below version 1: the first three words, data_size 24.
    assert_eq!(
        packet(cmd::WRITE, &words(&[0x10_0000, 0x600, 0x2000])).to_vec(),
        pad("2100 0000 1800 0000100000000000 0006000000000000 0020000000000000")
    );
    // U:781-798: READ_LBA {offset, size}.
    assert_eq!(
        packet(cmd::READ, &words(&[0x200, 0x400])).to_vec(),
        pad("2300 0000 1000 0002000000000000 0004000000000000")
    );
    // U:369-380, 395: VERIFY_LBA {offset, size}.
    assert_eq!(
        packet(cmd::VERIFY, &words(&[0, 0x600])).to_vec(),
        pad("2500 0000 1000 0000000000000000 0006000000000000")
    );
    // U:484-501: REBOOT with the u64 mark 0x52626F74.
    assert_eq!(
        packet(cmd::REBOOT, &0x5262_6f74u64.to_le_bytes()).to_vec(),
        pad("0100 0000 0800 746f625200000000")
    );
}
