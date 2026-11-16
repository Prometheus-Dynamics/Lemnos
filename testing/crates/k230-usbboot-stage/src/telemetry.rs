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
