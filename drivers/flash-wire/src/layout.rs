//! The layout hash: one 64-bit FNV-1a per family of the wire, computed at compile time from the
//! declarations in the code. Nothing here is a number kept by hand. A change to a constant, an op, a
//! status, a reset value, the record's head, the USB identity or the timing contract moves the hash of
//! the family that holds it, and so the hash of the whole wire.
//!
//! The families:
//! - [`FAMILY_FRAME`]: the magic, the sizes, the CRC, the reserved bits and the timing contract.
//! - [`FAMILY_OPS`]: the ops, their numbers and the shape of each reply.
//! - [`FAMILY_STATUS`]: the status bytes.
//! - [`FAMILY_RESET`]: the reset kinds.
//! - [`FAMILY_IDENTITY`]: the identity record's head and the way it lists families, its build id and
//!   its regions.
//! - [`FAMILY_USB`]: the device's identity: VID:PID, interface, endpoints, packet sizes, product string.
//!
//! A peer that pins [`WIRE_HASH`] agrees with this crate on every family. A peer that compares the families
//! one by one (see [`crate::identity::check`]) refuses a skew by name.

use crate::frame::{
    CRC_INIT, CRC_POLY, MAGIC_REQUEST, MAX_SECTORS, Op, PAD_BYTE, REQUEST_LEN, RESERVED_COMPARED,
    RESET_MASKROM, RESET_NORMAL, SECTOR, STATUS_BAD_REQUEST, STATUS_CRC, STATUS_NOT_COMPARED,
    STATUS_OK, STATUS_STORAGE, STATUS_VERIFY,
};
use crate::hash::{OFFSET, bytes, text, word};
use crate::identity::{IDENTITY_MAGIC, MAX_PEER_ENTRIES};
use crate::region::MAX_REGIONS;
use crate::timing::{
    FEED_TO_FEED_US, FIRST_REQUEST_WINDOW_US, HOST_BULK_TIMEOUT_US, IDLE_LIMIT_US, INIT_BUDGET_US,
    REFERENCE_WATCHDOG_PERIOD_US, REPLY_MARGIN_US, REPLY_READ_BOUND_US, ROUND_BUDGET_US,
    ROUND_PHASES, TRANSFER_BUDGET_US, WRITE_TO_FEED_PHASES,
};
use crate::usb::{
    BULK_IN, BULK_MAX_PACKET, BULK_OUT, CONFIGURATION_VALUE, EP0_MAX_PACKET, INTERFACE_CLASS,
    INTERFACE_NAME_PREFIX, INTERFACE_PROTOCOL, INTERFACE_SUBCLASS, LANGUAGE, PRODUCT, PRODUCT_ID,
    VENDOR_ID,
};

/// The family of the frame layout and the timing contract.
pub const FAMILY_FRAME: u8 = 1;
/// The family of the ops and their replies.
pub const FAMILY_OPS: u8 = 2;
/// The family of the status bytes.
pub const FAMILY_STATUS: u8 = 3;
/// The family of the reset kinds.
pub const FAMILY_RESET: u8 = 4;
/// The family of the identity record.
pub const FAMILY_IDENTITY: u8 = 5;
/// The family of the USB identity.
pub const FAMILY_USB: u8 = 6;

/// How many families the wire has. The identity record lists each one.
pub const FAMILY_COUNT: usize = 6;

/// The families in the order the identity record lists them.
pub const FAMILIES: [u8; FAMILY_COUNT] = [
    FAMILY_FRAME,
    FAMILY_OPS,
    FAMILY_STATUS,
    FAMILY_RESET,
    FAMILY_IDENTITY,
    FAMILY_USB,
];

/// The ops, with the shape of each reply. Each row folds into [`FAMILY_OPS`].
const OPS: [(&str, Op, &str); 4] = [
    (
        "read",
        Op::Read,
        "one status byte, then on ok the sectors and their crc16be; on a failure the status alone",
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
const STATUSES: [(&str, u8); 6] = [
    ("ok", STATUS_OK),
    ("crc", STATUS_CRC),
    ("storage", STATUS_STORAGE),
    ("verify", STATUS_VERIFY),
    ("not_compared", STATUS_NOT_COMPARED),
    ("bad_request", STATUS_BAD_REQUEST),
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
    h = text(
        h,
        "request: op u8, reserved u8, count u16le, lba u32le, crc16be over the first 10",
    );
    h = text(h, "chunk: payload, crc16be over the payload");
    h = text(
        h,
        "a bad request or a bad chunk: status, session continues; a link cut ends it",
    );
    // The timing contract: the same numbers on both sides.
    h = word(text(h, "transfer_budget_us"), TRANSFER_BUDGET_US);
    h = word(text(h, "round_phases"), ROUND_PHASES);
    h = word(text(h, "pad_byte"), PAD_BYTE as u64);
    h = word(text(h, "write_to_feed_phases"), WRITE_TO_FEED_PHASES);
    h = word(text(h, "round_budget_us"), ROUND_BUDGET_US);
    h = word(text(h, "idle_limit_us"), IDLE_LIMIT_US);
    h = word(text(h, "feed_to_feed_us"), FEED_TO_FEED_US);
    h = word(
        text(h, "reference_watchdog_period_us"),
        REFERENCE_WATCHDOG_PERIOD_US,
    );
    h = word(text(h, "host_bulk_timeout_us"), HOST_BULK_TIMEOUT_US);
    h = word(text(h, "reply_read_bound_us"), REPLY_READ_BOUND_US);
    h = word(text(h, "init_budget_us"), INIT_BUDGET_US);
    h = word(text(h, "first_request_window_us"), FIRST_REQUEST_WINDOW_US);
    h = word(text(h, "reply_margin_us"), REPLY_MARGIN_US);
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
    text(h, "other values: normal")
}

const fn identity_hash() -> u64 {
    let mut h = family_seed(FAMILY_IDENTITY, "identity");
    h = bytes(text(h, "magic.identity"), &IDENTITY_MAGIC);
    h = text(h, "head: len u16le (whole record), count u8");
    h = text(h, "entry: family u8, hash u64le");
    h = text(
        h,
        "then: build id u64le; region count u8; MAX_REGIONS x (first u32le, end u32le, class u8)",
    );
    h = text(
        h,
        "a reader skips trailing bytes it does not know; a family listed twice is refused",
    );
    h = word(text(h, "max_regions"), MAX_REGIONS as u64);
    h = word(text(h, "max_peer_entries"), MAX_PEER_ENTRIES as u64);
    h
}

const fn usb_hash() -> u64 {
    let mut h = family_seed(FAMILY_USB, "usb");
    h = word(text(h, "vendor_id"), VENDOR_ID as u64);
    h = word(text(h, "product_id"), PRODUCT_ID as u64);
    h = word(text(h, "interface_class"), INTERFACE_CLASS as u64);
    h = word(text(h, "interface_subclass"), INTERFACE_SUBCLASS as u64);
    h = word(text(h, "interface_protocol"), INTERFACE_PROTOCOL as u64);
    h = word(text(h, "configuration_value"), CONFIGURATION_VALUE as u64);
    h = word(text(h, "bulk_out"), BULK_OUT as u64);
    h = word(text(h, "bulk_in"), BULK_IN as u64);
    h = word(text(h, "bulk_max_packet"), BULK_MAX_PACKET as u64);
    h = word(text(h, "ep0_max_packet"), EP0_MAX_PACKET as u64);
    h = word(text(h, "language"), LANGUAGE as u64);
    h = text(h, "product string");
    h = text(h, PRODUCT);
    h = text(h, "interface string prefix");
    h = text(h, INTERFACE_NAME_PREFIX);
    h
}

/// The hash of each family, in the order of [`FAMILIES`].
pub const FAMILY_HASHES: [u64; FAMILY_COUNT] = [
    frame_hash(),
    ops_hash(),
    status_hash(),
    reset_hash(),
    identity_hash(),
    usb_hash(),
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

/// A short number derived from [`WIRE_HASH`], for naming an image or a log line. It identifies nothing on its
/// own: compare the full hash.
pub const WIRE_SHORT: u16 =
    (WIRE_HASH ^ (WIRE_HASH >> 16) ^ (WIRE_HASH >> 32) ^ (WIRE_HASH >> 48)) as u16;

#[cfg(test)]
mod tests {
    use super::*;

    // The values this build computes. A layout change moves them: change them in the same commit as the
    // layout, and say so in the README's table of hashes. Each was recomputed outside this crate by an
    // independent FNV walk over the same bytes (see the README).
    const PINNED_WIRE_HASH: u64 = 0x642D_1B44_FB2E_5496;
    const PINNED_WIRE_SHORT: u16 = 53457;
    const PINNED_FAMILY_HASHES: [u64; FAMILY_COUNT] = [
        0x274F_4492_2B2A_47C4,
        0xC170_D815_DF17_757F,
        0xE2B5_4BEB_21DA_BF76,
        0x40FE_F3FE_A154_445F,
        0x2065_1D23_2C11_F17D,
        0x8F59_A043_B5C3_0419,
    ];

    #[test]
    fn the_wire_hash_is_pinned() {
        assert_eq!(
            WIRE_HASH, PINNED_WIRE_HASH,
            "the wire's layout changed: see drivers/flash-wire/README.md"
        );
    }

    #[test]
    fn each_family_hash_is_pinned() {
        assert_eq!(FAMILY_HASHES, PINNED_FAMILY_HASHES);
    }

    #[test]
    fn the_short_number_is_derived_from_the_hash() {
        assert_eq!(WIRE_SHORT, PINNED_WIRE_SHORT);
        let fold16 = |h: u64| (h ^ (h >> 16) ^ (h >> 32) ^ (h >> 48)) as u16;
        assert_eq!(WIRE_SHORT, fold16(WIRE_HASH));
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

    #[test]
    fn the_wire_hash_is_derived_from_the_pinned_families_by_a_runtime_walk() {
        // The const fold of each family (its pin) is checked above. This walks the wire level again at run
        // time, over the family ids and pins, so the wire pin cannot be typed wrong either.
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
    }
}
