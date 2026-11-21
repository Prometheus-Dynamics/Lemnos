//! One piece of a card flash, carried by one boot-ROM load. The stage's own USB device path has not run
//! on hardware, so the image bytes reach the stage the only way the ROM gives: loaded into SRAM beside
//! the stage. The host loads the stage image at its load address and a job at [`JOB_ADDRESS`]: a header,
//! then up to [`MAX_SECTORS`] sectors of payload. The stage arms the watchdog first, starts the card,
//! refuses a card that is not the one the job names, does the one job, prints the outcome on the
//! console, and takes its SoC reset back to the ROM.
//!
//! The watchdog is fed after a sector is written, read back and equal, or read and hashed. Nothing
//! else feeds it.

use sha2::{Digest, Sha256};

use crate::format::crc32;
use crate::sd::{BLOCK, Card, SdError, SdHost};

/// Where the job's header is loaded: 128 KiB above the stage's load address. The stage image's footprint
/// must end below it (`check.sh` and `tests/image_link.rs`).
pub const JOB_ADDRESS: u32 = 0x8038_0000;
/// The payload's offset from the header.
pub const DATA_OFFSET: usize = 0x1000;
/// The most sectors a job can carry: the load range ends at `0x80400000`.
pub const MAX_SECTORS: u32 =
    ((0x8040_0000 - JOB_ADDRESS) as usize - DATA_OFFSET) as u32 / BLOCK as u32;
/// The header's length: sixteen little-endian words.
pub const HEADER_LEN: usize = 64;
/// The header's magic: the bytes `LPC1`.
pub const MAGIC: u32 = 0x3143_504c;
/// The most sectors a [`Op::Test`] job may name: their original content is held on the stack.
pub const TEST_SECTORS: usize = 8;
/// The most times one sector's write or read is tried.
pub const TRIES: u32 = 3;
/// Flag: raise the card clock to the data rate after the card has started.
pub const FAST: u32 = 1 << 0;

/// What a job does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// Write the payload's sectors, each read back and compared.
    Write = 1,
    /// Write zero sectors, each read back and compared. No payload.
    Zero = 2,
    /// Read the sectors and hash them. Nothing is written.
    Hash = 3,
    /// The bounded write test: keep the range's content, write the payload, read it back, write the
    /// content back, read it back.
    Test = 4,
}

/// A job's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Job {
    pub op: Op,
    pub lba: u64,
    pub count: u32,
    /// CRC-32 of the payload as the host sent it (0 for a job without one).
    pub data_crc: u32,
    pub flags: u32,
    /// The card the job is for: its size in sectors and its CID. Any other card is refused.
    pub card_blocks: u64,
    pub cid: [u8; 16],
}

impl Job {
    /// Whether the job carries a payload.
    pub fn has_payload(&self) -> bool {
        matches!(self.op, Op::Write | Op::Test)
    }

    /// The header's bytes.
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut words = [0u32; 16];
        words[0] = MAGIC;
        words[1] = self.op as u32;
        words[2] = self.lba as u32;
        words[3] = (self.lba >> 32) as u32;
        words[4] = self.count;
        words[5] = self.data_crc;
        words[6] = self.flags;
        words[7] = self.card_blocks as u32;
        words[8] = (self.card_blocks >> 32) as u32;
        for i in 0..4 {
            let b = &self.cid[4 * i..4 * i + 4];
            words[9 + i] = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        }
        let mut out = [0u8; HEADER_LEN];
        for (i, word) in words.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&word.to_le_bytes());
        }
        let crc = crc32(&out[..HEADER_LEN - 4]);
        out[HEADER_LEN - 4..].copy_from_slice(&crc.to_le_bytes());
        out
    }

    /// Reads a header. A wrong magic, CRC, operation or size is refused by name: a refused job does nothing.
    pub fn parse(bytes: &[u8; HEADER_LEN]) -> Result<Self, &'static str> {
        let word = |i: usize| {
            u32::from_le_bytes([
                bytes[4 * i],
                bytes[4 * i + 1],
                bytes[4 * i + 2],
                bytes[4 * i + 3],
            ])
        };
        if word(0) != MAGIC {
            return Err("piece refused: no job header at the job address");
        }
        if word(15) != crc32(&bytes[..HEADER_LEN - 4]) {
            return Err("piece refused: the job header's CRC is wrong");
        }
        let op = match word(1) {
            1 => Op::Write,
            2 => Op::Zero,
            3 => Op::Hash,
            4 => Op::Test,
            _ => return Err("piece refused: unknown operation"),
        };
        let mut cid = [0u8; 16];
        for i in 0..4 {
            cid[4 * i..4 * i + 4].copy_from_slice(&word(9 + i).to_be_bytes());
        }
        let job = Self {
            op,
            lba: u64::from(word(2)) | (u64::from(word(3)) << 32),
            count: word(4),
            data_crc: word(5),
            flags: word(6),
            card_blocks: u64::from(word(7)) | (u64::from(word(8)) << 32),
            cid,
        };
        if job.count == 0 {
            return Err("piece refused: no sectors");
        }
        if job.has_payload() && job.count > MAX_SECTORS {
            return Err("piece refused: more sectors than a load can carry");
        }
        if job.op == Op::Test && job.count as usize > TEST_SECTORS {
            return Err("piece refused: the write test is at most eight sectors");
        }
        Ok(job)
    }
}

/// Why a piece stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PieceError {
    /// The job was refused before any card write, with the reason.
    Refused(&'static str),
    /// The card or the host failed at this sector.
    Card(u64, SdError),
    /// The sector read back differs from what was written. The watchdog was not fed for it.
    Mismatch(u64),
}

/// What a finished piece reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// SHA-256 of the sectors as read from the card: after the write, or the hashed range.
    pub digest: [u8; 32],
    /// For the write test: SHA-256 of the range before the test, and of the range read back after the
    /// original content was written back.
    pub before_after: Option<([u8; 32], [u8; 32])>,
    /// Transfers that failed and were tried again (each ended in a sector that verified).
    pub retries: u32,
}

/// Refuses a card that is not the one the job names, a range past it, and a payload that is not what
/// the host sent. Called before any write.
pub fn check<H: SdHost>(card: &Card<H>, job: &Job, data: &[u8]) -> Result<(), PieceError> {
    if card.cid()[..15] != job.cid[..15] || card.blocks() != job.card_blocks {
        return Err(PieceError::Refused(
            "piece refused: not the card the job names",
        ));
    }
    match job.lba.checked_add(u64::from(job.count)) {
        Some(end) if end <= card.blocks() => {}
        _ => {
            return Err(PieceError::Refused(
                "piece refused: the range runs past the card",
            ));
        }
    }
    if job.has_payload()
        && (data.len() != job.count as usize * BLOCK || crc32(data) != job.data_crc)
    {
        return Err(PieceError::Refused(
            "piece refused: the payload is not what the host sent",
        ));
    }
    Ok(())
}

/// Tries a card operation up to [`TRIES`] times. After an error the host and the card are brought back
/// ([`Card::recover`]) before the next try; each failed try is counted. Bench, 2026-11-18: at 25 MHz on one data line a write's
/// data phase failed its CRC about once in 14,000 sectors; the card drops such a block, so it is sent again.
fn tried<H: SdHost, T>(
    card: &mut Card<H>,
    lba: u64,
    retries: &mut u32,
    mut op: impl FnMut(&mut Card<H>) -> Result<T, SdError>,
) -> Result<T, PieceError> {
    let mut last = None;
    for _ in 0..TRIES {
        match op(card) {
            Ok(value) => return Ok(value),
            Err(e) => {
                *retries += 1;
                last = Some(e);
                if let Err(e) = card.recover() {
                    return Err(PieceError::Card(lba, e));
                }
            }
        }
    }
    Err(PieceError::Card(
        lba,
        last.unwrap_or(SdError::Data("no try was made")),
    ))
}

/// Writes a sector and reads it back. A sector that reads back different is not tried again: that is a
/// failure of the card, not of a transfer.
fn write_checked<H: SdHost>(
    card: &mut Card<H>,
    lba: u64,
    block: &[u8; BLOCK],
    retries: &mut u32,
) -> Result<(), PieceError> {
    tried(card, lba, retries, |c| c.write(lba, block))?;
    let mut back = [0u8; BLOCK];
    tried(card, lba, retries, |c| c.read(lba, &mut back))?;
    if back != *block {
        return Err(PieceError::Mismatch(lba));
    }
    Ok(())
}

fn sector(data: &[u8], i: usize) -> [u8; BLOCK] {
    let mut block = [0u8; BLOCK];
    block.copy_from_slice(&data[i * BLOCK..(i + 1) * BLOCK]);
    block
}

/// Does the job. `fed` is called with the count of sectors done, after each sector that was written and
/// read back equal or read and hashed: the caller feeds the watchdog there and nowhere else.
pub fn run<H: SdHost>(
    card: &mut Card<H>,
    job: &Job,
    data: &[u8],
    fed: &mut dyn FnMut(u32),
) -> Result<Outcome, PieceError> {
    check(card, job, data)?;
    let mut hasher = Sha256::new();
    let mut before_after = None;
    let mut retries = 0;
    match job.op {
        Op::Hash => {
            for i in 0..job.count {
                let lba = job.lba + u64::from(i);
                let mut block = [0u8; BLOCK];
                card.read(lba, &mut block)
                    .map_err(|e| PieceError::Card(lba, e))?;
                hasher.update(block);
                fed(i + 1);
            }
        }
        Op::Write | Op::Zero => {
            for i in 0..job.count {
                let lba = job.lba + u64::from(i);
                let block = if job.op == Op::Write {
                    sector(data, i as usize)
                } else {
                    [0u8; BLOCK]
                };
                write_checked(card, lba, &block, &mut retries)?;
                hasher.update(block);
                fed(i + 1);
            }
        }
        Op::Test => {
            let count = job.count as usize;
            let mut kept = [[0u8; BLOCK]; TEST_SECTORS];
            let mut before = Sha256::new();
            for (i, slot) in kept.iter_mut().enumerate().take(count) {
                let lba = job.lba + i as u64;
                tried(card, lba, &mut retries, |c| c.read(lba, slot))?;
                before.update(*slot);
                fed(i as u32 + 1);
            }
            let mut failed = None;
            for i in 0..count {
                let block = sector(data, i);
                match write_checked(card, job.lba + i as u64, &block, &mut retries) {
                    Ok(()) => {
                        hasher.update(block);
                        fed(i as u32 + 1);
                    }
                    Err(e) => {
                        failed = Some(e);
                        break;
                    }
                }
            }
            // The content goes back whether or not the pattern held. The first failure is the one
            // reported: a failed restore after a failed write would hide the cause.
            let mut after = Sha256::new();
            for (i, slot) in kept.iter().enumerate().take(count) {
                match write_checked(card, job.lba + i as u64, slot, &mut retries) {
                    Ok(()) => {
                        after.update(*slot);
                        fed(i as u32 + 1);
                    }
                    Err(e) => {
                        failed.get_or_insert(e);
                    }
                }
            }
            if let Some(e) = failed {
                return Err(e);
            }
            before_after = Some((before.finalize().into(), after.finalize().into()));
        }
    }
    Ok(Outcome {
        digest: hasher.finalize().into(),
        before_after,
        retries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> Job {
        Job {
            op: Op::Write,
            lba: 0x1_0000_0002,
            count: 7,
            data_crc: 0x1234_5678,
            flags: FAST,
            card_blocks: 124_735_488,
            cid: *b"\x03SDSR64G\x80\x14\xa7\xc9\xab\x01\x51\x00",
        }
    }

    #[test]
    fn a_header_survives_the_load_and_a_damaged_one_is_refused_by_name() {
        let bytes = job().encode();
        assert_eq!(Job::parse(&bytes), Ok(job()));
        for at in [0, 5, 17, 40, 63] {
            let mut bad = bytes;
            bad[at] ^= 0x10;
            assert!(Job::parse(&bad).is_err(), "byte {at}");
        }
        // SRAM with no job in it.
        assert_eq!(
            Job::parse(&[0; HEADER_LEN]),
            Err("piece refused: no job header at the job address")
        );
    }

    #[test]
    fn sizes_a_load_cannot_carry_are_refused() {
        let mut big = job();
        big.count = MAX_SECTORS + 1;
        assert!(Job::parse(&big.encode()).is_err());
        let mut zero = big;
        zero.op = Op::Zero;
        assert!(
            Job::parse(&zero.encode()).is_ok(),
            "a zero fill carries no payload"
        );
        let mut test = job();
        test.op = Op::Test;
        test.count = 9;
        assert!(Job::parse(&test.encode()).is_err());
        assert_eq!(MAX_SECTORS, 1016);
    }
}
