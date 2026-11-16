//! Stage L's report: the card's boot area, read and judged, and the pad dump, sent to the host as records.
//! Nothing is written. The stage's device answers the queued records and feeds no watchdog: the report
//! ends with the board's watchdog return.
//!
//! Verdicts (the reviewer's ruling; "unknown" is not a verdict):
//! - **Bootable**: the magic `K230T1` at sector 2048 or at sector 3072. Then the dump of both 4 KiB header
//!   blocks is sent too (strings are extracted on the host). Stage B may run next.
//! - **Not bootable, blank**: no magic, and both 4 KiB blocks are all `0xff` or all zero. Stage R may run.
//! - **Not bootable, contents unrecognised**: no magic, and the blocks hold other bytes. Stage R may run.
//! - **Read error**: a card read failed. The run stops (the card is reported as a fault candidate).
//!
//! The magic `K230T1` is the probe image's own first-stage header, checked on the file (sectors 2048 and
//! 3072). Which copy the ROM reads, and whether the ROM reads only that header, are UNVERIFIED.
//!
//! The stage makes no SD access at all unless the pad gate (`pads::sd_pads_on_sd_function`) passes.

use crate::format::{self, PACKET};
use crate::sd::{BLOCK, Card, SdError, SdHost};
use crate::stage::{NoReply, Reboot};
use crate::telemetry;
use crate::usb_dev::{BulkEndpoints, Device, UsbHw};

/// The first-stage copies' sectors on SD: 2048 (1 MiB) and 3072 (1536 KiB).
pub const COPY_SECTORS: [u64; 2] = [2048, 3072];
/// The header block of each copy read for the dump and the blank test: 4 KiB, eight sectors.
pub const HEADER_SECTORS: u64 = 8;
/// The header block's length in bytes.
pub const HEADER_LEN: usize = 4096;
/// The firmware header's length: the payload starts at byte 528.
pub const FW_HEADER_LEN: usize = 528;
/// The firmware header's magic: bytes 0 to 3.
pub const FW_MAGIC: &[u8; 4] = b"K230";
/// The largest payload length a header may claim and still be "present" (a few MiB, far below the card).
pub const FW_MAX_PAYLOAD: u64 = 4 << 20;
/// The most of a payload the stage reads and hashes per copy. A longer payload is "not checked".
pub const FW_HASH_CAP: u64 = 1 << 20;
/// The encryption types the header may carry: 0 none, 1 and 2 the vendor's other forms (`firmware_gen.py`
/// lines 372 and 409 set them). Anything else is not a header this stage knows.
pub const FW_KNOWN_TYPES: [u32; 3] = [0, 1, 2];
/// The most records one report holds: boot, eight pads, the card report, two firmware headers, and the dumps.
pub const MAX_RECORDS: usize = 1 + 8 + 1 + 2 + 2 * (HEADER_LEN / telemetry::DUMP_BYTES + 1);

/// The firmware header's fields, as the bytes give them (`firmware_gen.py`, the `-n` branch: length, type,
/// then `hash_format`'s 32-byte hash and 484 zero bytes; the magic is written before them, per the image).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FwHeader {
    pub magic: bool,
    pub length: u32,
    pub kind: u32,
    pub hash: [u8; 32],
}

/// Reads the header fields from the first 528 bytes of a copy.
pub fn parse_header(bytes: &[u8]) -> FwHeader {
    let magic = bytes.len() >= 4 && &bytes[0..4] == FW_MAGIC;
    let le32 = |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let mut hash = [0u8; 32];
    if bytes.len() >= 44 {
        hash.copy_from_slice(&bytes[12..44]);
    }
    FwHeader {
        magic,
        length: if bytes.len() >= 8 { le32(4) } else { 0 },
        kind: if bytes.len() >= 12 { le32(8) } else { 0 },
        hash,
    }
}

/// Whether the header is "present": the magic, a sane length (non-zero, at most [`FW_MAX_PAYLOAD`], and inside
/// the card), and a known type. Each is reported, so a header that is present but not sane is seen as such.
pub fn header_present(h: &FwHeader, card_bytes: u64) -> bool {
    h.magic
        && h.length > 0
        && u64::from(h.length) <= FW_MAX_PAYLOAD
        && FW_HEADER_LEN as u64 + u64::from(h.length) <= card_bytes
        && FW_KNOWN_TYPES.contains(&h.kind)
}

/// What the payload's hash said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadCheck {
    /// The SHA-256 of the payload is the stored one.
    Verified,
    /// The payload's SHA-256 is not the stored one. The ROM would presumably reject it (UNVERIFIED): the stage
    /// treats it as bootable for Stage B's purposes, and blanks it.
    Mismatch,
    /// The payload is longer than the cap, so it was not hashed.
    NotChecked,
}

/// One copy, judged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CopyInfo {
    pub header: FwHeader,
    pub present: bool,
    pub check: Option<PayloadCheck>,
}

/// The verdict on the two copies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// A header is present and its payload hash verifies.
    BootableVerified,
    /// A header is present and its payload hash does not match (Stage B's branch: blanked).
    HeaderMismatch,
    /// A header is present, its payload is over the cap and was not hashed (Stage B's branch).
    HeaderUnchecked,
    /// No header, and both 4 KiB blocks are blank.
    NotBootableBlank,
    /// No header, and the blocks hold other bytes (Stage R may run).
    NotBootableUnrecognised,
    /// A card read failed.
    ReadError,
}

impl Verdict {
    /// The code the card record carries.
    pub fn code(self) -> u8 {
        match self {
            Self::BootableVerified => 1,
            Self::NotBootableBlank => 2,
            Self::NotBootableUnrecognised => 3,
            Self::ReadError => 4,
            Self::HeaderMismatch => 7,
            Self::HeaderUnchecked => 8,
        }
    }

    /// Whether Stage B's branch applies (a header is present, whatever its hash says).
    pub fn bootable(self) -> bool {
        matches!(self, Self::BootableVerified | Self::HeaderMismatch | Self::HeaderUnchecked)
    }
}

/// The judgement of one copy, from its 4 KiB block. `hash_of(n)` hashes the first `n` payload bytes, on the card
/// or in a test; it is called only when a header is present and its payload is within the cap. Pure apart from
/// that call, so the rules are tested without a card.
pub fn judge_copy(
    block: &[u8],
    card_bytes: u64,
    hash_of: &mut dyn FnMut(u64) -> Result<[u8; 32], SdError>,
) -> Result<CopyInfo, SdError> {
    let header = parse_header(block);
    let present = header_present(&header, card_bytes);
    let check = if !present {
        None
    } else if u64::from(header.length) > FW_HASH_CAP {
        Some(PayloadCheck::NotChecked)
    } else if hash_of(u64::from(header.length))? == header.hash {
        Some(PayloadCheck::Verified)
    } else {
        Some(PayloadCheck::Mismatch)
    };
    Ok(CopyInfo {
        header,
        present,
        check,
    })
}

/// The verdict on the two copies.
pub fn verdict(copies: &[CopyInfo; 2], blocks: &[[u8; HEADER_LEN]; 2]) -> Verdict {
    if copies.iter().any(|c| c.check == Some(PayloadCheck::Verified)) {
        Verdict::BootableVerified
    } else if copies.iter().any(|c| c.check == Some(PayloadCheck::Mismatch)) {
        Verdict::HeaderMismatch
    } else if copies.iter().any(|c| c.present) {
        Verdict::HeaderUnchecked
    } else if blocks.iter().all(|b| is_blank(b)) {
        Verdict::NotBootableBlank
    } else {
        Verdict::NotBootableUnrecognised
    }
}

/// Whether a block is blank: all `0xff`, or all zero.
pub fn is_blank(block: &[u8]) -> bool {
    block.iter().all(|&b| b == 0xff) || block.iter().all(|&b| b == 0)
}

/// The two header blocks, read from the card, with the copies' judgements.
pub struct BootAreas {
    pub blocks: [[u8; HEADER_LEN]; 2],
    pub copies: [CopyInfo; 2],
    pub verdict: Verdict,
}

/// Judges two header blocks already in memory (a sim or a test, with no card). A payload that fits inside the
/// block is hashed from it; a longer payload is "not checked", since the rest is on a card this does not have.
pub fn judge_blocks(blocks: [[u8; HEADER_LEN]; 2], card_bytes: u64) -> BootAreas {
    let in_block = (HEADER_LEN - FW_HEADER_LEN) as u64;
    let mut copies = [CopyInfo {
        header: parse_header(&[]),
        present: false,
        check: None,
    }; 2];
    for (i, block) in blocks.iter().enumerate() {
        let header = parse_header(block);
        let present = header_present(&header, card_bytes);
        let check = if !present {
            None
        } else if u64::from(header.length) > in_block {
            Some(PayloadCheck::NotChecked)
        } else {
            let start = FW_HEADER_LEN;
            let end = start + header.length as usize;
            let hash: [u8; 32] = {
                use sha2::{Digest, Sha256};
                Sha256::digest(&block[start..end]).into()
            };
            Some(if hash == header.hash { PayloadCheck::Verified } else { PayloadCheck::Mismatch })
        };
        copies[i] = CopyInfo { header, present, check };
    }
    let verdict = verdict(&copies, &blocks);
    BootAreas { blocks, copies, verdict }
}

/// Reads and judges both copies: the 4 KiB block of each, then, when a header is present, the payload's hash,
/// read from the card and streamed through SHA-256 (never stored; at most [`FW_HASH_CAP`] bytes).
pub fn read_boot_areas<H: SdHost>(card: &mut Card<H>) -> Result<BootAreas, SdError> {
    let card_bytes = card.blocks() * BLOCK as u64;
    let mut blocks = [[0u8; HEADER_LEN]; 2];
    for (block, sector) in blocks.iter_mut().zip(COPY_SECTORS) {
        for i in 0..HEADER_SECTORS {
            let mut sec = [0u8; BLOCK];
            card.read(sector + i, &mut sec)?;
            block[i as usize * BLOCK..(i as usize + 1) * BLOCK].copy_from_slice(&sec);
        }
    }
    let mut copies = [CopyInfo {
        header: parse_header(&[]),
        present: false,
        check: None,
    }; 2];
    for (i, sector) in COPY_SECTORS.iter().enumerate() {
        let block = blocks[i];
        copies[i] = judge_copy(&block, card_bytes, &mut |n| hash_payload_on_card(card, *sector, &block, n))?;
    }
    let verdict = verdict(&copies, &blocks);
    Ok(BootAreas {
        blocks,
        copies,
        verdict,
    })
}

/// The SHA-256 of the first `n` payload bytes of the copy at `sector`. The payload starts at byte 528 of the
/// copy; the bytes of the header block are in memory, and the rest are read from the card, sector by sector,
/// after the header block's eight sectors.
fn hash_payload_on_card<H: SdHost>(
    card: &mut Card<H>,
    sector: u64,
    block: &[u8; HEADER_LEN],
    n: u64,
) -> Result<[u8; 32], SdError> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut left = n;
    let in_block = (HEADER_LEN - FW_HEADER_LEN).min(left as usize);
    hasher.update(&block[FW_HEADER_LEN..FW_HEADER_LEN + in_block]);
    left -= in_block as u64;
    let mut next = sector + HEADER_SECTORS;
    while left > 0 {
        let mut sec = [0u8; BLOCK];
        card.read(next, &mut sec)?;
        let take = (BLOCK as u64).min(left) as usize;
        hasher.update(&sec[..take]);
        left -= take as u64;
        next += 1;
    }
    Ok(hasher.finalize().into())
}

/// The records of one report, built into a fixed array (the stage has no allocator).
pub struct Records {
    pub packets: [[u8; PACKET]; MAX_RECORDS],
    pub count: usize,
}

/// Why the card has no verdict, when it has none: the reasons the stage reports before any read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoCard {
    /// The pad gate refused: the SD pads are not on the SD function as the ROM left them. No SD access was made.
    PadGate,
    /// No SD controller was given to the stage (the host's parameter), and the stage does not choose one.
    NotGiven,
    /// The card did not answer or a read failed (a card fault candidate).
    Failed,
}

impl NoCard {
    pub fn code(self) -> u8 {
        match self {
            Self::Failed => 4,
            Self::PadGate => 5,
            Self::NotGiven => 6,
        }
    }
}

/// The card's state for the report: read, or no verdict and the reason.
pub enum CardState<'a> {
    Read(&'a BootAreas),
    Absent(NoCard),
}

/// The flags of a copy's firmware header, as the record carries them.
fn fw_flags(c: &CopyInfo) -> u8 {
    let mut f = 0u8;
    if c.header.magic {
        f |= 1;
    }
    if c.present {
        f |= 2;
    }
    match c.check {
        Some(PayloadCheck::Verified) => f |= 4,
        Some(PayloadCheck::Mismatch) => f |= 8,
        Some(PayloadCheck::NotChecked) => f |= 16,
        None => {}
    }
    f
}

/// The card-report flags: bit 0 magic at 2048, bit 1 at 3072, bit 2 blank at 2048, bit 3 blank at 3072.
pub fn card_flags(areas: &BootAreas) -> u8 {
    let mut f = 0u8;
    for (i, c) in areas.copies.iter().enumerate() {
        if c.header.magic {
            f |= 1 << i;
        }
        if is_blank(&areas.blocks[i]) {
            f |= 1 << (2 + i);
        }
    }
    f
}

/// The CRC-32 of each 4 KiB header block.
pub fn crcs(areas: &BootAreas) -> [u32; 2] {
    [format::crc32(&areas.blocks[0]), format::crc32(&areas.blocks[1])]
}

/// Builds the report's records: the boot record, the pad records, the card report, the firmware header of each
/// copy, and, when a header is present, the dumps of both 4 KiB header blocks (strings are read on the host).
pub fn build(
    boot_ctl: u32,
    pads: &[u32; crate::pads::PAD_COUNT],
    cid: &[u8; 16],
    blocks: u64,
    card: CardState<'_>,
) -> Records {
    let mut out = Records {
        packets: [[0u8; PACKET]; MAX_RECORDS],
        count: 0,
    };
    let push = |out: &mut Records, packet: [u8; PACKET]| {
        out.packets[out.count] = packet;
        out.count += 1;
    };
    push(&mut out, telemetry::boot(boot_ctl));
    for index in 0..8u8 {
        let mut words = [0u32; 8];
        words.copy_from_slice(&pads[8 * index as usize..8 * index as usize + 8]);
        push(&mut out, telemetry::pad(index, &words));
    }
    match card {
        CardState::Absent(reason) => {
            push(&mut out, telemetry::card_report(cid, blocks, reason.code(), 0, [0, 0]));
        }
        CardState::Read(areas) => {
            push(
                &mut out,
                telemetry::card_report(cid, blocks, areas.verdict.code(), card_flags(areas), crcs(areas)),
            );
            for (region, c) in areas.copies.iter().enumerate() {
                push(
                    &mut out,
                    telemetry::fw_header(region as u8, fw_flags(c), c.header.length, c.header.kind, &c.header.hash),
                );
            }
            if areas.verdict.bootable() {
                for (region, block) in areas.blocks.iter().enumerate() {
                    for (chunk_at, chunk) in block.chunks(telemetry::DUMP_BYTES).enumerate() {
                        let offset = (chunk_at * telemetry::DUMP_BYTES) as u32;
                        push(&mut out, telemetry::dump(region as u8, offset, chunk));
                    }
                }
            }
        }
    }
    out
}

/// The bulk side of a report: each host read takes the next record, and a bus reset starts again from the
/// first. A command from the host is ignored: the report is read, not driven.
pub struct ReportBulk<'a> {
    pub records: &'a Records,
    pub next: usize,
}

impl BulkEndpoints for ReportBulk<'_> {
    fn on_out(&mut self, _message: &[u8]) -> Result<(), Reboot> {
        Ok(())
    }

    fn on_in(&mut self, out: &mut [u8]) -> Result<usize, NoReply> {
        if self.next >= self.records.count {
            return Err(NoReply);
        }
        out[..PACKET].copy_from_slice(&self.records.packets[self.next]);
        self.next += 1;
        Ok(PACKET)
    }

    fn on_reset(&mut self) {
        self.next = 0;
    }
}

/// Answers the records forever. It feeds no watchdog and makes no write: the board's watchdog returns it. A
/// device that does not start leaves the loop idle, which is the same return.
pub fn serve<H: UsbHw>(hw: H, records: &Records) -> ! {
    let mut bulk = ReportBulk { records, next: 0 };
    let mut device = match Device::start(hw) {
        Ok(device) => Some(device),
        Err(_) => None,
    };
    loop {
        if let Some(device) = device.as_mut() {
            let _ = device.step(&mut bulk);
        } else {
            core::hint::spin_loop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pads::PAD_COUNT;
    use crate::telemetry::{Record, decode};
    use sha2::{Digest, Sha256};

    /// A firmware header built by the rule `firmware_gen.py -n` follows (length, type, hash, zeros to 528), with
    /// the magic `K230` at 0, then the payload from byte 528.
    fn build_copy(payload: &[u8], kind: u32, stored_hash: Option<[u8; 32]>) -> Vec<u8> {
        let mut bytes = vec![0u8; FW_HEADER_LEN];
        bytes[0..4].copy_from_slice(FW_MAGIC);
        bytes[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes[8..12].copy_from_slice(&kind.to_le_bytes());
        let hash: [u8; 32] = stored_hash.unwrap_or_else(|| Sha256::digest(payload).into());
        bytes[12..44].copy_from_slice(&hash);
        bytes.extend_from_slice(payload);
        bytes
    }

    fn judge(bytes: &[u8]) -> CopyInfo {
        let mut block = [0u8; HEADER_LEN];
        let n = bytes.len().min(HEADER_LEN);
        block[..n].copy_from_slice(&bytes[..n]);
        let payload = bytes.get(FW_HEADER_LEN..).unwrap_or(&[]).to_vec();
        judge_copy(&block, 1 << 30, &mut |n| {
            Ok(Sha256::digest(&payload[..(n as usize).min(payload.len())]).into())
        })
        .unwrap()
    }

    fn areas(first: &[u8], second: &[u8]) -> BootAreas {
        let mut blocks = [[0u8; HEADER_LEN]; 2];
        blocks[0][..first.len().min(HEADER_LEN)].copy_from_slice(&first[..first.len().min(HEADER_LEN)]);
        blocks[1][..second.len().min(HEADER_LEN)].copy_from_slice(&second[..second.len().min(HEADER_LEN)]);
        let copies = [judge(first), judge(second)];
        let verdict = verdict(&copies, &blocks);
        BootAreas { blocks, copies, verdict }
    }

    #[test]
    fn a_header_built_by_the_rule_with_a_verified_hash_is_bootable() {
        let copy = build_copy(&[7u8; 300], 0, None);
        let a = areas(&copy, &[]);
        assert_eq!(a.copies[0].check, Some(PayloadCheck::Verified));
        assert_eq!(a.verdict, Verdict::BootableVerified);
        assert!(a.verdict.bootable());
        assert_eq!(card_flags(&a) & 1, 1, "magic at 2048");
    }

    #[test]
    fn a_header_with_a_wrong_hash_is_present_and_mismatched_and_still_bootable_for_stage_b() {
        let copy = build_copy(&[7u8; 300], 0, Some([0xaa; 32]));
        let a = areas(&copy, &[]);
        assert_eq!(a.copies[0].check, Some(PayloadCheck::Mismatch));
        assert_eq!(a.verdict, Verdict::HeaderMismatch);
        assert!(a.verdict.bootable(), "stage B blanks it");
    }

    #[test]
    fn bytes_four_and_five_spelling_t1_are_not_the_magic() {
        // The old reading was wrong: "T1" at byte 4 is the length field of this header, not a magic.
        let mut bytes = vec![0u8; FW_HEADER_LEN + 16];
        bytes[0..4].copy_from_slice(b"XXXX");
        bytes[4..6].copy_from_slice(b"T1");
        let info = judge(&bytes);
        assert!(!info.header.magic);
        assert!(!info.present);
        assert_eq!(info.check, None);
    }

    #[test]
    fn a_header_whose_length_is_insane_or_whose_type_is_unknown_is_not_present() {
        let mut zero_len = build_copy(&[], 0, None);
        zero_len[4..8].copy_from_slice(&0u32.to_le_bytes());
        assert!(!judge(&zero_len).present);
        let unknown_type = build_copy(&[1u8; 8], 9, None);
        assert!(!judge(&unknown_type).present);
        let huge = {
            let mut b = build_copy(&[1u8; 8], 0, None);
            b[4..8].copy_from_slice(&((FW_MAX_PAYLOAD + 1) as u32).to_le_bytes());
            b
        };
        assert!(!judge(&huge).present);
    }

    #[test]
    fn a_card_with_no_header_is_not_bootable_and_says_whether_it_is_blank_or_unrecognised() {
        let blank = areas(&[0xff; HEADER_LEN], &[0xff; HEADER_LEN]);
        assert_eq!(blank.verdict, Verdict::NotBootableBlank);
        assert!(!blank.verdict.bootable());
        let other = areas(b"not a header", &[]);
        assert_eq!(other.verdict, Verdict::NotBootableUnrecognised);
    }

    #[test]
    fn a_bootable_report_carries_the_firmware_headers_and_both_dumps_and_a_blank_one_does_not() {
        let pads = [0u32; PAD_COUNT];
        let cid = [0x5c; 16];
        let bootable = areas(&build_copy(&[3u8; 64], 0, None), &[]);
        let r = build(3, &pads, &cid, 1024, CardState::Read(&bootable));
        let decoded: Vec<Record> = r.packets[..r.count].iter().map(|p| decode(p).unwrap()).collect();
        let fw = decoded.iter().filter(|x| matches!(x, Record::FwHeader { .. })).count();
        assert_eq!(fw, 2, "one firmware-header record per copy");
        let dumps = decoded.iter().filter(|x| matches!(x, Record::Dump { .. })).count();
        assert_eq!(dumps, 2 * HEADER_LEN / telemetry::DUMP_BYTES + 2, "both 4 KiB blocks, in chunks");
        let blank = areas(&[0xff; HEADER_LEN], &[0xff; HEADER_LEN]);
        let r = build(3, &pads, &cid, 1024, CardState::Read(&blank));
        assert_eq!(r.count, 1 + 8 + 1 + 2, "boot, pads, card report, two firmware headers: no dumps");
        assert!(r.count <= MAX_RECORDS);
    }

    #[test]
    fn every_record_decodes_and_the_pad_records_carry_all_sixty_four_registers() {
        let mut pads = [0u32; PAD_COUNT];
        for (i, p) in pads.iter_mut().enumerate() {
            *p = 0x100 + i as u32;
        }
        let r = build(9, &pads, &[1; 16], 8, CardState::Absent(NoCard::PadGate));
        let mut seen = [0u32; PAD_COUNT];
        let mut boots = 0;
        for packet in &r.packets[..r.count] {
            match decode(packet).expect("every record decodes") {
                Record::Pad { index, words } => {
                    seen[8 * index as usize..8 * index as usize + 8].copy_from_slice(&words);
                }
                Record::Boot { ctl } => {
                    assert_eq!(ctl, 9);
                    boots += 1;
                }
                Record::CardReport { verdict, blocks, .. } => {
                    assert_eq!(verdict, NoCard::PadGate.code());
                    assert_eq!(blocks, 8);
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(boots, 1);
        assert_eq!(seen, pads);
    }
}
