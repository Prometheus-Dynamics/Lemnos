//! The layout hash: one 64-bit FNV-1a per family of the wire, computed at compile time from the
//! declarations in the code. Nothing here is a number kept by hand. A change to a constant, an op,
//! a status, a reset value, the record's head or its family list moves the hash of the family that
//! holds it, and so the hash of the whole wire.
//!
//! The families:
//! - [`FAMILY_FRAME`]: the magic, the sizes, the CRC.
//! - [`FAMILY_OPS`]: the ops, their numbers and the shape of each reply.
//! - [`FAMILY_STATUS`]: the status bytes.
//! - [`FAMILY_RESET`]: the reset kinds.
//! - [`FAMILY_IDENTITY`]: the identity record's head and the way it lists families.
//!
//! A peer that pins [`WIRE_HASH`] agrees with this crate on every family. A peer that compares the
//! families one by one (see [`crate::identity::check`]) refuses a skew by name.

use crate::frame::{
    CRC_INIT, CRC_POLY, MAGIC_REQUEST, MAX_SECTORS, Op, REQUEST_LEN, RESERVED_COMPARED,
    RESET_MASKROM, RESET_NORMAL, SECTOR, STATUS_CRC, STATUS_NOT_COMPARED, STATUS_OK,
    STATUS_STORAGE, STATUS_VERIFY,
};
use crate::hash::{OFFSET, bytes, text, word};
use crate::identity::IDENTITY_MAGIC;

/// The family of the frame layout.
pub const FAMILY_FRAME: u8 = 1;
/// The family of the ops and their replies.
pub const FAMILY_OPS: u8 = 2;
/// The family of the status bytes.
pub const FAMILY_STATUS: u8 = 3;
/// The family of the reset kinds.
pub const FAMILY_RESET: u8 = 4;
/// The family of the identity record.
pub const FAMILY_IDENTITY: u8 = 5;

/// How many families the wire has. The identity record lists each one.
pub const FAMILY_COUNT: usize = 5;

/// The families in the order the identity record lists them.
pub const FAMILIES: [u8; FAMILY_COUNT] = [
    FAMILY_FRAME,
    FAMILY_OPS,
    FAMILY_STATUS,
    FAMILY_RESET,
    FAMILY_IDENTITY,
];

/// The ops, with the shape of each reply. Each row folds into [`FAMILY_OPS`].
const OPS: [(&str, Op, &str); 4] = [
    (
        "read",
        Op::Read,
        "sectors then crc16be; a status is not sent",
    ),
    (
        "write",
        Op::Write,
        "chunk (payload then crc16be) from the host; one status byte back",
    ),
    (
        "end",
        Op::End,
        "one status byte back; the reserved byte is the reset kind",
    ),
    ("identify", Op::Identify, "the identity record back"),
];

/// The status bytes. Each row folds into [`FAMILY_STATUS`].
const STATUSES: [(&str, u8); 5] = [
    ("ok", STATUS_OK),
    ("crc", STATUS_CRC),
    ("storage", STATUS_STORAGE),
    ("verify", STATUS_VERIFY),
    ("not_compared", STATUS_NOT_COMPARED),
];

/// The reset kinds. Each row folds into [`FAMILY_RESET`].
const RESETS: [(&str, u8); 2] = [("normal", RESET_NORMAL), ("maskrom", RESET_MASKROM)];

const fn family_seed(family: u8, name: &str) -> u64 {
    text(word(OFFSET, family as u64), name)
}

const fn frame_hash() -> u64 {
    let mut h = family_seed(FAMILY_FRAME, "frame");
    h = bytes(text(h, "magic.request"), &MAGIC_REQUEST);
    h = word(text(h, "sector"), SECTOR as u64);
    h = word(text(h, "max_sectors"), MAX_SECTORS as u64);
    h = word(text(h, "request_len"), REQUEST_LEN as u64);
    h = word(text(h, "reserved.compared"), RESERVED_COMPARED as u64);
    h = word(text(h, "crc.poly"), CRC_POLY as u64);
    h = word(text(h, "crc.init"), CRC_INIT as u64);
    // The request's field order and widths, and the chunk's CRC placement, as the encoder writes them.
    h = text(
        h,
        "request: op u8, reserved u8, count u16le, lba u32le, crc16be over the first 10",
    );
    h = text(h, "chunk: payload, crc16be over the payload");
    h
}

const fn ops_hash() -> u64 {
    let mut h = family_seed(FAMILY_OPS, "ops");
    let mut i = 0;
    while i < OPS.len() {
        let (name, op, shape) = OPS[i];
        h = word(text(h, name), op.to_wire() as u64);
        h = text(h, shape);
        i += 1;
    }
    h
}

const fn status_hash() -> u64 {
    let mut h = family_seed(FAMILY_STATUS, "status");
    let mut i = 0;
    while i < STATUSES.len() {
        let (name, value) = STATUSES[i];
        h = word(text(h, name), value as u64);
        i += 1;
    }
    h
}

const fn reset_hash() -> u64 {
    let mut h = family_seed(FAMILY_RESET, "reset");
    let mut i = 0;
    while i < RESETS.len() {
        let (name, value) = RESETS[i];
        h = word(text(h, name), value as u64);
        i += 1;
    }
    // Any other byte is the normal reset: a rule, folded in with the values.
    text(h, "other values: normal")
}

const fn identity_hash() -> u64 {
    let mut h = family_seed(FAMILY_IDENTITY, "identity");
    h = bytes(text(h, "magic.identity"), &IDENTITY_MAGIC);
    h = text(h, "head: len u16le (whole record), count u8");
    h = text(h, "entry: family u8, hash u64le");
    h = text(h, "a reader skips trailing bytes it does not know");
    h
}

/// The hash of each family, in the order of [`FAMILIES`].
pub const FAMILY_HASHES: [u64; FAMILY_COUNT] = [
    frame_hash(),
    ops_hash(),
    status_hash(),
    reset_hash(),
    identity_hash(),
];

const fn wire_hash() -> u64 {
    let mut h = OFFSET;
    let mut i = 0;
    while i < FAMILY_COUNT {
        h = word(h, FAMILIES[i] as u64);
        h = word(h, FAMILY_HASHES[i]);
        i += 1;
    }
    h
}

/// The hash of the whole wire: the families and their hashes, in order. A PC side pins this.
pub const WIRE_HASH: u64 = wire_hash();

/// A short number derived from [`WIRE_HASH`], for naming an image or a log line. It identifies nothing
/// on its own: compare the full hash.
pub const WIRE_SHORT: u16 =
    (WIRE_HASH ^ (WIRE_HASH >> 16) ^ (WIRE_HASH >> 32) ^ (WIRE_HASH >> 48)) as u16;

#[cfg(test)]
mod tests {
    use super::*;

    // The values this build computes. A layout change moves them: change them in the same commit as the
    // layout, and say so in the README's table of hashes.
    const PINNED_WIRE_HASH: u64 = 0x2DED_8934_37C3_D947;
    const PINNED_WIRE_SHORT: u16 = 19037;
    const PINNED_FAMILY_HASHES: [u64; FAMILY_COUNT] = [
        0x7E49_7C0D_EC42_79FF,
        0x89BF_43BE_BC6E_C950,
        0x246D_E5A0_33B0_58C9,
        0x40FE_F3FE_A154_445F,
        0xB71D_E980_887B_B9AD,
    ];

    #[test]
    fn the_wire_hash_is_pinned() {
        assert_eq!(
            WIRE_HASH, PINNED_WIRE_HASH,
            "the wire's layout changed: see docs in drivers/flash-wire/README.md"
        );
    }

    #[test]
    fn each_family_hash_is_pinned() {
        assert_eq!(FAMILY_HASHES, PINNED_FAMILY_HASHES);
    }

    #[test]
    fn the_short_number_is_derived_from_the_hash() {
        assert_eq!(WIRE_SHORT, PINNED_WIRE_SHORT);
        assert_eq!(WIRE_SHORT, fold16(WIRE_HASH));
    }

    fn fold16(hash: u64) -> u16 {
        (hash ^ (hash >> 16) ^ (hash >> 32) ^ (hash >> 48)) as u16
    }

    #[test]
    fn the_wire_hash_is_derived_from_the_pinned_families_by_a_runtime_walk() {
        // The const fold of each family (its pin) is checked above. This walks the wire level again at
        // run time, over the family ids and pins, so the wire pin cannot be typed wrong either.
        for (i, family) in FAMILIES.iter().enumerate() {
            assert_eq!(FAMILY_HASHES[i], PINNED_FAMILY_HASHES[i], "family {family}");
        }
        let mut wire = 0xcbf2_9ce4_8422_2325u64;
        for (i, family) in FAMILIES.iter().enumerate() {
            for b in [*family as u64, PINNED_FAMILY_HASHES[i]] {
                for byte in b.to_le_bytes() {
                    wire ^= u64::from(byte);
                    wire = wire.wrapping_mul(0x0000_0100_0000_01b3);
                }
            }
        }
        assert_eq!(wire, PINNED_WIRE_HASH);
        assert_eq!(WIRE_SHORT, fold16(wire));
    }

    #[test]
    fn a_family_hash_moves_with_its_own_declaration_only() {
        // Rebuild the reset family by hand with one value changed: the hash must move.
        let mut h = family_seed(FAMILY_RESET, "reset");
        h = word(text(h, "normal"), RESET_NORMAL as u64);
        h = word(text(h, "maskrom"), 4);
        h = text(h, "other values: normal");
        assert_ne!(h, FAMILY_HASHES[3]);
    }
}
