//! The boot header's write, cut at every byte. The header is the commit point: a cut anywhere in
//! its write must leave either the old (invalidated) header, which the ROM refuses, or the new,
//! verified header over the payload it names. Never a header that boots the wrong bytes.

mod common;

use lemnos_k230_usbboot::bootrom::{self, Stage};
use lemnos_k230_usbboot::burner::{Burner, Medium};
use lemnos_k230_usbboot::flash::{self, HEADER_OFFSET, PAYLOAD_OFFSET, retry_until};
use lemnos_k230_usbboot::sim::{BOOT_US, BoardState, SimBoard, SimConfig, sim_stage_image};
use lemnos_k230_usbboot_stage::format::{BOOT_HEADER_LEN, BootHeader, rom_accepts};
use lemnos_k230_usbboot_stage::wdt::CLOCK_HZ_MAX;
use sha2::{Digest, Sha256};

const LOAD: u32 = 0x8036_0000;

fn image() -> Vec<u8> {
    (0..3000u32).map(|i| (i * 31 % 253) as u8).collect()
}

/// A board with the stage up, over a medium of 1 MiB.
fn stage_board() -> SimBoard {
    let mut board = SimBoard::new(&SimConfig {
        clock_hz: CLOCK_HZ_MAX,
        ..SimConfig::default()
    });
    let clock = board.clock();
    retry_until(
        60_000,
        || match bootrom::stage(&mut board) {
            Ok(Stage::BootRom) => Some(()),
            _ => None,
        },
        |ms| clock.advance_us(u64::from(ms) * 1000),
    )
    .unwrap();
    bootrom::load_and_start(&mut board, &sim_stage_image(), LOAD).unwrap();
    board
}

/// The medium operations of the commit, as a clean run makes them: `(first, end)`, the header write
/// and its read-backs. The header block goes to the medium in bulk pages, and each page is written
/// and read back by the stage, so the header is several operations.
fn header_ops() -> (u64, u64) {
    let mut board = stage_board();
    let clock = board.clock();
    let img = image();
    let mut burner = Burner::new(&mut board);
    flash::payload(&mut burner, Medium::Sd, &img).unwrap();
    let first = clock.ops();
    flash::commit(&mut burner, &img).unwrap();
    (first, clock.ops())
}

/// The bulk page the host sends a header in (`lemnos_k230_usbboot::bootrom::PAGE`).
const PAGE: u64 = 1000;

/// Cuts the medium operation `op` after `bytes` of it, then checks the medium: no header, or the new
/// one over the verified payload, and the board's own answer agrees with the header.
fn cut_at_op(op: u64, bytes: u64) -> bool {
    let img = image();
    let mut board = stage_board();
    board.cut_at(op, bytes);
    let mut burner = Burner::new(&mut board);
    flash::payload(&mut burner, Medium::Sd, &img).ok();
    let _ = flash::commit(&mut burner, &img);
    let medium = board.medium();
    let header = &medium[HEADER_OFFSET as usize..HEADER_OFFSET as usize + BOOT_HEADER_LEN];
    let booted = rom_accepts(header);
    if booted {
        // A header the ROM would boot names the verified payload, byte for byte.
        let decoded = BootHeader::decode(header).expect("a header the ROM accepts decodes");
        assert_eq!(decoded.payload_offset, PAYLOAD_OFFSET);
        assert_eq!(decoded.payload_size, img.len() as u64);
        let payload = &medium[PAYLOAD_OFFSET as usize..PAYLOAD_OFFSET as usize + img.len()];
        assert_eq!(
            payload,
            img.as_slice(),
            "a booted header names bytes that are not the image"
        );
        assert_eq!(decoded.digest, <[u8; 32]>::from(Sha256::digest(&img)));
    }
    if !booted {
        board.clock().advance_us(BOOT_US);
    }
    let state = board.state();
    assert_eq!(
        state == BoardState::Booted,
        booted,
        "the ROM disagrees with the header at op {op}, byte {bytes}"
    );
    booted
}

#[test]
fn a_cut_at_every_byte_of_the_header_write_leaves_no_wrong_bootable_header() {
    let (verify, end) = header_ops();
    // The commit's VERIFY reads first (one operation). The stage then reads the header block to see
    // whether a valid header is there (`check_range`), a second operation. The header's first page
    // is the third, and the header is complete at byte 512 of that page.
    let first = verify + 2;
    let mut first_boot = None;
    for op in first..end {
        for bytes in 0..=PAGE {
            if cut_at_op(op, bytes) && first_boot.is_none() {
                first_boot = Some((op, bytes));
            }
        }
    }
    // Real-failing: a booting cut is one where the header's 512 bytes are whole, which is the first
    // page's 512th byte at the earliest. No cut before that boots.
    assert_eq!(
        first_boot,
        Some((first, BOOT_HEADER_LEN as u64)),
        "a header was valid before its last byte was written"
    );
}

#[test]
fn a_header_that_was_written_whole_reads_back_as_written() {
    // The flash's read-back of the header is the last check. A complete header reads back equal.
    let img = image();
    let mut board = stage_board();
    let mut burner = Burner::new(&mut board);
    flash::flash(&mut burner, Medium::Sd, &img).unwrap();
    let medium = board.medium();
    assert!(rom_accepts(&medium[..BOOT_HEADER_LEN]));
}
