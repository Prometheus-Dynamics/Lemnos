//! The stage, over a RAM medium, with the feeds counted. Every refusal the stage makes is checked,
//! and the feed rule is checked on each path: a feed only after a verified chunk.

use lemnos_k230_usbboot_stage::format::{
    self, BOOT_HEADER_LEN, BootHeader, HEADER_BLOCK, REBOOT_MARK, cmd, crc32, packet,
};
use lemnos_k230_usbboot_stage::medium::{Geometry, Medium, MediumError};
use lemnos_k230_usbboot_stage::ram::RamMedium;
use lemnos_k230_usbboot_stage::stage::{Reboot, Stage};
use lemnos_k230_usbboot_stage::wdt::Feed;
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::rc::Rc;

/// Counts the feeds. Clones share the count, so a test reads it after the stage has fed.
#[derive(Clone, Default)]
struct Feeds(Rc<Cell<u32>>);

impl Feeds {
    fn count(&self) -> u32 {
        self.0.get()
    }
}

impl Feed for Feeds {
    fn feed(&mut self) {
        self.0.set(self.0.get() + 1);
    }
    fn fast_reset(&mut self) {}
}

/// A medium that keeps only half of every write and reports success: a fault the read-back must see.
struct Halving<'a>(RamMedium<'a>);

impl Medium for Halving<'_> {
    fn geometry(&self) -> Geometry {
        self.0.geometry()
    }
    fn read(&mut self, offset: u64, out: &mut [u8]) -> Result<(), MediumError> {
        self.0.read(offset, out)
    }
    fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), MediumError> {
        let half = data.len() / 2;
        self.0.write(offset, &data[..half])
    }
    fn erase(&mut self, offset: u64, len: u64) -> Result<(), MediumError> {
        self.0.erase(offset, len)
    }
    fn permits(&self, offset: u64, len: u64) -> Result<(), MediumError> {
        self.0.permits(offset, len)
    }
}

const BLOCK: u64 = 512;
const ERASE: u64 = 4096;

/// A command, sent and answered: the reply's command, result and data.
fn command<M: Medium, F: Feed>(
    stage: &mut Stage<M, F>,
    command: u16,
    data: &[u8],
) -> (u16, u16, Vec<u8>) {
    let _ = stage.on_out(&packet(command, data));
    let mut out = [0u8; 60];
    let n = stage.on_in(&mut out).expect("a reply");
    assert_eq!(n, 60);
    let size = usize::from(format::le16(&out, 4));
    (
        format::le16(&out, 0),
        format::le16(&out, 2),
        out[6..6 + size].to_vec(),
    )
}

fn words(values: &[u64]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// A write of `data` at `offset`: the start, the data pieces of `piece` bytes, then the end reply.
fn write<M: Medium, F: Feed>(
    stage: &mut Stage<M, F>,
    offset: u64,
    data: &[u8],
    piece: usize,
) -> (u16, u16, Vec<u8>) {
    let total = (data.len() as u64).div_ceil(BLOCK) * BLOCK;
    let start = command(stage, cmd::WRITE, &words(&[offset, total, 0, 0]));
    assert_eq!(start.1, format::RESULT_OK, "the write start");
    let mut padded = data.to_vec();
    padded.resize(total as usize, 0);
    for chunk in padded.chunks(piece) {
        let _ = stage.on_out(chunk);
    }
    let mut out = [0u8; 60];
    stage.on_in(&mut out).expect("the write's end reply");
    let size = usize::from(format::le16(&out, 4));
    (
        format::le16(&out, 0),
        format::le16(&out, 2),
        out[6..6 + size].to_vec(),
    )
}

fn medium(bytes: &mut [u8]) -> RamMedium<'_> {
    RamMedium::new(bytes, BLOCK, ERASE)
}

#[test]
fn crc32_is_the_standard_check_value() {
    assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
}

#[test]
fn a_header_round_trips_and_a_damaged_one_is_refused() {
    let header = BootHeader {
        payload_offset: HEADER_BLOCK,
        payload_size: 1234,
        digest: [7; 32],
    };
    let bytes = header.encode();
    assert!(format::rom_accepts(&bytes));
    assert_eq!(BootHeader::decode(&bytes), Some(header));
    let mut damaged = bytes;
    damaged[40] ^= 1;
    assert!(!format::rom_accepts(&damaged));
    assert_eq!(BootHeader::decode(&damaged), None);
    let mut other_version = bytes;
    other_version[12] = 2;
    let crc = crc32(&{
        let mut copy = other_version;
        copy[4..8].fill(0);
        copy
    });
    other_version[4..8].copy_from_slice(&crc.to_le_bytes());
    assert!(format::rom_accepts(&other_version));
    assert_eq!(
        BootHeader::decode(&other_version),
        None,
        "a version this stage does not decode"
    );
}

#[test]
fn probe_and_info_describe_the_medium_and_the_stage_chunk() {
    let mut bytes = vec![0xffu8; 1 << 16];
    let feeds = Feeds::default();
    let mut stage = Stage::new(medium(&mut bytes), feeds.clone());
    let (c, r, data) = command(&mut stage, cmd::PROBE, &[2, 0xff]);
    assert_eq!((c, r), (cmd::PROBE | format::REPLY_FLAG, format::RESULT_OK));
    assert_eq!(format::le64(&data, 0), 4096);
    assert_eq!(format::le64(&data, 8), 4096);
    let (_, r, data) = command(&mut stage, cmd::GET_INFO, &[]);
    assert_eq!(r, format::RESULT_OK);
    assert_eq!(format::le64(&data, 0), 1 << 16);
    assert_eq!(
        (format::le64(&data, 8), format::le64(&data, 16)),
        (BLOCK, ERASE)
    );
    assert_eq!(format::le64(&data, 24) >> 47 & 1, 1, "valid");
}

#[test]
fn a_probe_with_no_medium_is_refused_with_its_reason() {
    let mut bytes = vec![0xffu8; 1 << 16];
    let feeds = Feeds::default();
    let mut stage = Stage::new(medium(&mut bytes), feeds.clone());
    let (_, r, data) = command(&mut stage, cmd::PROBE, &[]);
    assert_eq!(r, format::RESULT_MESSAGE);
    assert!(String::from_utf8_lossy(&data).contains("one medium"));
}

#[test]
fn the_full_commit_feeds_per_verified_piece_and_the_header_names_the_verified_payload() {
    let payload: Vec<u8> = (0..3000u32).map(|i| (i * 7 % 251) as u8).collect();
    let mut bytes = vec![0xffu8; 1 << 16];
    let feeds = Feeds::default();
    let mut stage = Stage::new(medium(&mut bytes), feeds.clone());
    // 1. Invalidate: erase the header block.
    assert_eq!(
        command(&mut stage, cmd::ERASE, &words(&[0, HEADER_BLOCK])).1,
        format::RESULT_OK
    );
    // 2. The payload, one piece: written, read back, fed.
    assert_eq!(
        write(&mut stage, HEADER_BLOCK, &payload, 4096).1,
        format::RESULT_OK
    );
    assert_eq!(feeds.count(), 1);
    // 3. The verify reads and hashes it, feeding per chunk.
    let (_, r, digest_reply) = command(&mut stage, cmd::VERIFY, &words(&[HEADER_BLOCK, 3000]));
    assert_eq!(r, format::RESULT_OK);
    let digest: [u8; 32] = Sha256::digest(&payload).into();
    assert_eq!(&digest_reply[..32], &digest);
    assert_eq!(feeds.count(), 2);
    // 4. The header, last, naming the verified payload.
    let header = BootHeader {
        payload_offset: HEADER_BLOCK,
        payload_size: 3000,
        digest,
    }
    .encode();
    let mut block = vec![0u8; HEADER_BLOCK as usize];
    block[..BOOT_HEADER_LEN].copy_from_slice(&header);
    assert_eq!(write(&mut stage, 0, &block, 4096).1, format::RESULT_OK);
    assert_eq!(feeds.count(), 3);
    drop(stage);
    assert!(format::rom_accepts(&bytes[..BOOT_HEADER_LEN]));
    assert_eq!(
        &bytes[HEADER_BLOCK as usize..HEADER_BLOCK as usize + 3000],
        &payload[..]
    );
}

#[test]
fn a_header_without_a_verify_of_its_payload_is_refused() {
    let mut bytes = vec![0xffu8; 1 << 16];
    let feeds = Feeds::default();
    let mut stage = Stage::new(medium(&mut bytes), feeds.clone());
    command(&mut stage, cmd::ERASE, &words(&[0, HEADER_BLOCK]));
    let header = BootHeader {
        payload_offset: HEADER_BLOCK,
        payload_size: 16,
        digest: [9; 32],
    }
    .encode();
    let mut block = vec![0u8; HEADER_BLOCK as usize];
    block[..BOOT_HEADER_LEN].copy_from_slice(&header);
    let (_, r, data) = write(&mut stage, 0, &block, 4096);
    assert_eq!(r, format::RESULT_MESSAGE);
    assert!(String::from_utf8_lossy(&data).contains("does not name"));
    assert_eq!(feeds.count(), 0, "a refused header is not fed");
    drop(stage);
    assert!(
        !format::rom_accepts(&bytes[..BOOT_HEADER_LEN]),
        "nothing was booted"
    );
}

#[test]
fn a_header_naming_another_digest_than_the_verified_one_is_refused() {
    let payload = vec![5u8; 1000];
    let mut bytes = vec![0xffu8; 1 << 16];
    let feeds = Feeds::default();
    let mut stage = Stage::new(medium(&mut bytes), feeds.clone());
    command(&mut stage, cmd::ERASE, &words(&[0, HEADER_BLOCK]));
    write(&mut stage, HEADER_BLOCK, &payload, 4096);
    command(&mut stage, cmd::VERIFY, &words(&[HEADER_BLOCK, 1000]));
    let header = BootHeader {
        payload_offset: HEADER_BLOCK,
        payload_size: 1000,
        digest: [0xee; 32],
    }
    .encode();
    let mut block = vec![0u8; HEADER_BLOCK as usize];
    block[..BOOT_HEADER_LEN].copy_from_slice(&header);
    let (_, r, _) = write(&mut stage, 0, &block, 4096);
    assert_eq!(r, format::RESULT_MESSAGE);
    drop(stage);
    assert!(!format::rom_accepts(&bytes[..BOOT_HEADER_LEN]));
}

#[test]
fn a_payload_write_under_a_valid_header_is_refused_until_the_header_is_erased() {
    // A medium that already holds a valid header (an earlier flash).
    let header = BootHeader {
        payload_offset: HEADER_BLOCK,
        payload_size: 8,
        digest: [1; 32],
    }
    .encode();
    let mut bytes = vec![0xffu8; 1 << 16];
    bytes[..BOOT_HEADER_LEN].copy_from_slice(&header);
    let feeds = Feeds::default();
    let mut stage = Stage::new(medium(&mut bytes), feeds.clone());
    let (_, r, data) = command(&mut stage, cmd::WRITE, &words(&[HEADER_BLOCK, 512, 0, 0]));
    assert_eq!(r, format::RESULT_MESSAGE, "a write under a valid header");
    assert!(String::from_utf8_lossy(&data).contains("erase it first"));
    let (_, r, _) = command(&mut stage, cmd::ERASE, &words(&[HEADER_BLOCK, ERASE]));
    assert_eq!(
        r,
        format::RESULT_MESSAGE,
        "an erase past the header is refused too"
    );
    assert_eq!(
        command(&mut stage, cmd::ERASE, &words(&[0, HEADER_BLOCK])).1,
        format::RESULT_OK
    );
    assert_eq!(
        write(&mut stage, HEADER_BLOCK, &[1u8; 512], 512).1,
        format::RESULT_OK
    );
}

#[test]
fn a_read_back_that_differs_is_an_error_and_is_not_fed() {
    let mut bytes = vec![0xffu8; 1 << 16];
    let feeds = Feeds::default();
    let mut stage = Stage::new(Halving(medium(&mut bytes)), feeds.clone());
    command(&mut stage, cmd::ERASE, &words(&[HEADER_BLOCK, ERASE]));
    let (_, r, data) = write(&mut stage, HEADER_BLOCK, &[3u8; 4096], 4096);
    assert_eq!(r, format::RESULT_MESSAGE);
    assert!(String::from_utf8_lossy(&data).contains("read-back differs"));
    assert_eq!(
        feeds.count(),
        0,
        "a piece that did not verify does not feed"
    );
}

#[test]
fn a_write_past_the_medium_or_off_the_erase_grid_is_refused() {
    let mut bytes = vec![0xffu8; 1 << 16];
    let feeds = Feeds::default();
    let mut stage = Stage::new(medium(&mut bytes), feeds.clone());
    let (_, r, _) = command(&mut stage, cmd::WRITE, &words(&[1 << 16, 512, 0, 0]));
    assert_eq!(r, format::RESULT_MESSAGE);
    let (_, r, _) = command(&mut stage, cmd::WRITE, &words(&[100, 512, 0, 0]));
    assert_eq!(r, format::RESULT_MESSAGE, "not on an erase block");
    let (_, r, _) = command(&mut stage, cmd::WRITE, &words(&[HEADER_BLOCK, 512, 0, 0]));
    assert_eq!(
        r,
        format::RESULT_OK,
        "a whole-block write after the header is fine"
    );
}

#[test]
fn the_reboot_needs_its_mark_and_then_asks_for_the_fast_reset() {
    let mut bytes = vec![0xffu8; 1 << 16];
    let feeds = Feeds::default();
    let mut stage = Stage::new(medium(&mut bytes), feeds.clone());
    let wrong = packet(cmd::REBOOT, &(REBOOT_MARK + 1).to_le_bytes());
    let _ = stage.on_out(&wrong);
    let mut out = [0u8; 60];
    assert!(
        stage.on_in(&mut out).is_ok(),
        "a refused reboot is answered"
    );
    assert_eq!(
        stage.on_out(&packet(cmd::REBOOT, &REBOOT_MARK.to_le_bytes())),
        Err(Reboot)
    );
}

#[test]
fn a_read_comes_back_in_chunks_then_its_end() {
    let mut bytes: Vec<u8> = (0..1u32 << 16).map(|i| (i % 253) as u8).collect();
    let expected = bytes.clone();
    let feeds = Feeds::default();
    let mut stage = Stage::new(medium(&mut bytes), feeds.clone());
    let (_, r, _) = command(&mut stage, cmd::READ, &words(&[100, 5000]));
    assert_eq!(r, format::RESULT_OK);
    let mut got = Vec::new();
    let mut buf = vec![0u8; 6 + 4096];
    let n = stage.on_in(&mut buf).expect("the first chunk");
    assert_eq!(n, 6 + 4096);
    got.extend_from_slice(&buf[6..n]);
    let n = stage.on_in(&mut buf[..6 + 904]).expect("the rest");
    assert_eq!(n, 6 + 904);
    got.extend_from_slice(&buf[6..n]);
    assert_eq!(got, expected[100..5100]);
    let mut end = [0u8; 60];
    stage.on_in(&mut end).expect("the end reply");
    assert_eq!(format::le16(&end, 0), cmd::READ_CHUNK | format::REPLY_FLAG);
}

#[test]
fn an_unknown_command_is_refused_and_a_short_packet_is_refused() {
    let mut bytes = vec![0xffu8; 1 << 16];
    let feeds = Feeds::default();
    let mut stage = Stage::new(medium(&mut bytes), feeds.clone());
    let (_, r, _) = command(&mut stage, 0x7f, &[]);
    assert_eq!(r, format::RESULT_MESSAGE);
    let _ = stage.on_out(&[0u8; 10]);
    let mut out = [0u8; 60];
    stage.on_in(&mut out).expect("answered");
    assert_eq!(format::le16(&out, 2), format::RESULT_MESSAGE);
    let _ = HEADER_BLOCK;
}
