//! The probe's telemetry records, sent on the bulk IN endpoint while a host listens. They are never a
//! condition of returning: a run with no host still returns by its path.
//!
//! A record is one 60-byte reply packet (`format::reply`): the command is [`SAMPLE`] or [`TAKING`],
//! the result is OK, and the data is the record:
//! - `SAMPLE`: the time CSR (u64, device ticks), then `wdt0`'s CR and CCVR and `wdt1`'s CR and CCVR
//!   (u32 each): 24 bytes.
//! - `TAKING`: the path the stage is about to take (u8: [`PATH_DEADLINE`] or [`PATH_SOC`]), then the time
//!   CSR (u64): 9 bytes. Only the paths the stage's own code triggers are named; a watchdog's reset
//!   is the hardware's, and no record is possible after it.

use crate::format::{self, PACKET, RESULT_OK};

/// A sample record's command.
pub const SAMPLE: u16 = 0x40;
/// A "taking <path> now" record's command.
pub const TAKING: u16 = 0x41;
/// The boot record's command: the board's `soc_boot_ctl`, read on the board, sent once per probe.
pub const BOOT: u16 = 0x42;
/// The card record's command: the card check (CID, capacity, two block CRCs), sent once per stage start.
pub const CARD: u16 = 0x43;
/// A pad record: eight raw pad registers (`IO8k` to `IO8k+7`) and the index `k` (0 to 7), from the read-only dump.
pub const PAD: u16 = 0x45;
/// The card's boot-area report: CID, capacity, the verdict and flags, and the CRCs of the two header blocks.
pub const CARD_REPORT: u16 = 0x46;
/// A dump of the boot area: 48 bytes of one header copy at an offset (the strings are extracted on the host).
pub const DUMP: u16 = 0x47;
/// The bytes of a dump record.
pub const DUMP_BYTES: usize = 48;
/// A copy's firmware header: region (0 at sector 2048, 1 at 3072), flags, the length, the type, and the stored hash.
pub const FW_HEADER: u16 = 0x48;
/// The path a deadline (the spin's time limit, then the SoC reset) takes.
pub const PATH_DEADLINE: u8 = 1;
/// The path the SoC reset takes at once.
pub const PATH_SOC: u8 = 2;

/// One decoded record, as the host reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Record {
    Sample {
        ticks: u64,
        cr: [u32; 2],
        ccvr: [u32; 2],
    },
    Taking {
        path: u8,
        ticks: u64,
    },
    Boot {
        ctl: u32,
    },
    Card {
        cid: [u8; 16],
        blocks: u64,
        crc_first: u32,
        crc_middle: u32,
    },
    Pad {
        index: u8,
        words: [u32; 8],
    },
    CardReport {
        cid: [u8; 16],
        blocks: u64,
        verdict: u8,
        flags: u8,
        crc: [u32; 2],
    },
    Dump {
        region: u8,
        offset: u32,
        len: u8,
        bytes: [u8; DUMP_BYTES],
    },
    FwHeader {
        region: u8,
        flags: u8,
        length: u32,
        kind: u32,
        hash: [u8; 32],
    },
}

/// A firmware-header packet. Flags: bit 0 the magic `K230`, bit 1 present (sane length, known type), bit 2 the
/// payload hash verified, bit 3 it mismatched, bit 4 it was not checked (over the cap).
pub fn fw_header(region: u8, flags: u8, length: u32, kind: u32, hash: &[u8; 32]) -> [u8; PACKET] {
    let mut data = [0u8; 42];
    data[0] = region;
    data[1] = flags;
    format::put32(&mut data, 2, length);
    format::put32(&mut data, 6, kind);
    data[10..42].copy_from_slice(hash);
    format::reply(FW_HEADER, RESULT_OK, &data)
}

/// A pad record's packet: `index` (0 to 7) and the eight registers `IO(8*index)` to `IO(8*index+7)`.
pub fn pad(index: u8, words: &[u32; 8]) -> [u8; PACKET] {
    let mut data = [0u8; 33];
    data[0] = index;
    for (i, w) in words.iter().enumerate() {
        format::put32(&mut data, 1 + 4 * i, *w);
    }
    format::reply(PAD, RESULT_OK, &data)
}

/// The card-report packet: CID, capacity, verdict, flags, and the CRCs of the two header blocks.
pub fn card_report(
    cid: &[u8; 16],
    blocks: u64,
    verdict: u8,
    flags: u8,
    crc: [u32; 2],
) -> [u8; PACKET] {
    let mut data = [0u8; 34];
    data[..16].copy_from_slice(cid);
    format::put64(&mut data, 16, blocks);
    data[24] = verdict;
    data[25] = flags;
    format::put32(&mut data, 26, crc[0]);
    format::put32(&mut data, 30, crc[1]);
    format::reply(CARD_REPORT, RESULT_OK, &data)
}

/// A dump packet: region (0 or 1), the offset within the 4 KiB header block, and up to 48 bytes.
pub fn dump(region: u8, offset: u32, bytes: &[u8]) -> [u8; PACKET] {
    let mut data = [0u8; 6 + DUMP_BYTES];
    data[0] = region;
    format::put32(&mut data, 1, offset);
    data[5] = bytes.len() as u8;
    data[6..6 + bytes.len()].copy_from_slice(bytes);
    format::reply(DUMP, RESULT_OK, &data[..6 + bytes.len()])
}

/// The card record's packet: the CID (16 bytes), the capacity in blocks, and the two block CRCs.
pub fn card(check: &crate::sd::CardCheck) -> [u8; PACKET] {
    let mut data = [0u8; 32];
    data[..16].copy_from_slice(&check.cid);
    format::put64(&mut data, 16, check.blocks);
    format::put32(&mut data, 24, check.crc_first);
    format::put32(&mut data, 28, check.crc_middle);
    format::reply(CARD, RESULT_OK, &data)
}

/// The boot record's packet: `soc_boot_ctl` as the stage read it.
pub fn boot(ctl: u32) -> [u8; PACKET] {
    let mut data = [0u8; 4];
    format::put32(&mut data, 0, ctl);
    format::reply(BOOT, RESULT_OK, &data)
}

/// A sample record's packet.
pub fn sample(ticks: u64, cr: [u32; 2], ccvr: [u32; 2]) -> [u8; PACKET] {
    let mut data = [0u8; 24];
    format::put64(&mut data, 0, ticks);
    for i in 0..2 {
        format::put32(&mut data, 8 + 8 * i, cr[i]);
        format::put32(&mut data, 12 + 8 * i, ccvr[i]);
    }
    format::reply(SAMPLE, RESULT_OK, &data)
}

/// A "taking <path> now" record's packet.
pub fn taking(path: u8, ticks: u64) -> [u8; PACKET] {
    let mut data = [0u8; 9];
    data[0] = path;
    format::put64(&mut data, 1, ticks);
    format::reply(TAKING, RESULT_OK, &data)
}

/// Decodes one record packet. Anything else (a command reply, a short packet) is `None`.
pub fn decode(packet: &[u8]) -> Option<Record> {
    if packet.len() != PACKET {
        return None;
    }
    let command = format::le16(packet, 0);
    let size = usize::from(format::le16(packet, 4));
    let data = &packet[format::PACKET_HEAD..format::PACKET_HEAD + size];
    match command {
        c if c == SAMPLE | format::REPLY_FLAG && size == 24 => Some(Record::Sample {
            ticks: format::le64(data, 0),
            cr: [format::le32(data, 8), format::le32(data, 16)],
            ccvr: [format::le32(data, 12), format::le32(data, 20)],
        }),
        c if c == PAD | format::REPLY_FLAG && size == 33 => {
            let mut words = [0u32; 8];
            for (i, w) in words.iter_mut().enumerate() {
                *w = format::le32(data, 1 + 4 * i);
            }
            Some(Record::Pad {
                index: data[0],
                words,
            })
        }
        c if c == CARD_REPORT | format::REPLY_FLAG && size == 34 => {
            let mut cid = [0u8; 16];
            cid.copy_from_slice(&data[..16]);
            Some(Record::CardReport {
                cid,
                blocks: format::le64(data, 16),
                verdict: data[24],
                flags: data[25],
                crc: [format::le32(data, 26), format::le32(data, 30)],
            })
        }
        c if c == FW_HEADER | format::REPLY_FLAG && size == 42 => {
            let mut hash = [0u8; 32];
            hash.copy_from_slice(&data[10..42]);
            Some(Record::FwHeader {
                region: data[0],
                flags: data[1],
                length: format::le32(data, 2),
                kind: format::le32(data, 6),
                hash,
            })
        }
        c if c == DUMP | format::REPLY_FLAG && size >= 5 => {
            let len = data[5] as usize;
            let mut bytes = [0u8; DUMP_BYTES];
            bytes[..len.min(DUMP_BYTES)].copy_from_slice(&data[6..6 + len.min(DUMP_BYTES)]);
            Some(Record::Dump {
                region: data[0],
                offset: format::le32(data, 1),
                len: data[5],
                bytes,
            })
        }
        c if c == CARD | format::REPLY_FLAG && size == 32 => {
            let mut cid = [0u8; 16];
            cid.copy_from_slice(&data[..16]);
            Some(Record::Card {
                cid,
                blocks: format::le64(data, 16),
                crc_first: format::le32(data, 24),
                crc_middle: format::le32(data, 28),
            })
        }
        c if c == BOOT | format::REPLY_FLAG && size == 4 => Some(Record::Boot {
            ctl: format::le32(data, 0),
        }),
        c if c == TAKING | format::REPLY_FLAG && size == 9 => Some(Record::Taking {
            path: data[0],
            ticks: format::le64(data, 1),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sample_round_trips() {
        let packet = sample(12_345, [1, 0x21], [0x0800_0000, 0x0400_0000]);
        assert_eq!(
            decode(&packet),
            Some(Record::Sample {
                ticks: 12_345,
                cr: [1, 0x21],
                ccvr: [0x0800_0000, 0x0400_0000],
            })
        );
    }

    #[test]
    fn a_taking_record_round_trips_and_a_short_or_other_packet_is_none() {
        assert_eq!(
            decode(&taking(PATH_DEADLINE, 99)),
            Some(Record::Taking {
                path: PATH_DEADLINE,
                ticks: 99
            })
        );
        assert_eq!(decode(&[0u8; 10]), None);
        assert_eq!(decode(&format::reply(0x00, RESULT_OK, &[])), None);
    }
}
