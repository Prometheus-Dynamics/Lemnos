//! The layout of each target's FEL thunk blob: where the code ends and each data word sits, the swap table's
//! size, and the stated version. The offsets are the symbols of `lemnos-driver-allwinner-fel-sys`'s assembly, and
//! `scripts/check-blob.sh` checks each one against the built object. The layout hash is folded from these
//! values, so a change to any of them moves the hash, and a PC side that pins the hash refuses a skew.
//!
//! Every data field is a 32-bit word, little-endian, at the offset given. The table is `table_entries` entries of
//! three words each: buf1, buf2, bytes. A zero byte count ends it; the list holds at most `table_entries - 1`
//! swaps, so the terminator always fits. The AArch64 save area sits on an 8-byte boundary, so up to four bytes of
//! padding can follow the last data word.

use crate::hash;

/// One target's blob layout. Every offset and length is in bytes, from the blob's first byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The Rust target the blob is built for.
    pub triple: &'static str,
    /// The stated version of this blob. It changes whenever the code or the layout changes.
    pub version: u32,
    /// The whole blob's length.
    pub blob_len: usize,
    /// The swap table's first byte.
    pub table_offset: usize,
    /// The swap table's entry count (three words each).
    pub table_entries: usize,
    /// The SPL entry word (a 32-bit address).
    pub entry_offset: usize,
    /// The pass flag, zero until the SPL is called.
    pub flag_offset: usize,
    /// The watchdog's register address, a 32-bit address. The thunk writes the arm value here first.
    pub wdt_addr_offset: usize,
    /// The value that arms the watchdog.
    pub wdt_arm_offset: usize,
    /// The value that disarms the watchdog before the thunk returns to the ROM.
    pub wdt_disarm_offset: usize,
    /// The register save area's first byte.
    pub save_offset: usize,
    /// The save area's length.
    pub save_len: usize,
}

impl Layout {
    /// The layout hash: FNV-1a over the target, the version and every offset, in this order.
    pub const fn hash(&self) -> u64 {
        let mut h = hash::text(hash::OFFSET, self.triple);
        h = hash::word(h, self.version as u64);
        h = hash::word(h, self.blob_len as u64);
        h = hash::word(h, self.table_offset as u64);
        h = hash::word(h, self.table_entries as u64);
        h = hash::word(h, self.entry_offset as u64);
        h = hash::word(h, self.flag_offset as u64);
        h = hash::word(h, self.wdt_addr_offset as u64);
        h = hash::word(h, self.wdt_arm_offset as u64);
        h = hash::word(h, self.wdt_disarm_offset as u64);
        h = hash::word(h, self.save_offset as u64);
        hash::word(h, self.save_len as u64)
    }
}

/// The ARMv7-A thunk, `armv7a-none-eabi`, ARM state.
pub const ARMV7: Layout = Layout {
    triple: "armv7a-none-eabi",
    version: 1,
    blob_len: 364,
    table_offset: 208,
    table_entries: 8,
    entry_offset: 304,
    flag_offset: 308,
    wdt_addr_offset: 312,
    wdt_arm_offset: 316,
    wdt_disarm_offset: 320,
    save_offset: 324,
    save_len: 40,
};

/// The AArch64 thunk, `aarch64-unknown-none`.
pub const AARCH64: Layout = Layout {
    triple: "aarch64-unknown-none",
    version: 1,
    blob_len: 456,
    table_offset: 232,
    table_entries: 8,
    entry_offset: 328,
    flag_offset: 332,
    wdt_addr_offset: 336,
    wdt_arm_offset: 340,
    wdt_disarm_offset: 344,
    save_offset: 352,
    save_len: 104,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_data_fills_the_blob_in_order() {
        for layout in [ARMV7, AARCH64] {
            // The table, then the entry, flag and watchdog words, then the save area, end at the blob's end.
            assert_eq!(
                layout.table_offset + layout.table_entries * 12,
                layout.entry_offset
            );
            assert_eq!(layout.entry_offset + 4, layout.flag_offset);
            assert_eq!(layout.flag_offset + 4, layout.wdt_addr_offset);
            assert_eq!(layout.wdt_addr_offset + 4, layout.wdt_arm_offset);
            assert_eq!(layout.wdt_arm_offset + 4, layout.wdt_disarm_offset);
            // The save area follows the last data word, after at most the padding that aligns it to 8.
            let pad = layout.save_offset - (layout.wdt_disarm_offset + 4);
            assert!(
                pad < 8,
                "{} has {pad} bytes before its save area",
                layout.triple
            );
            assert_eq!(layout.save_offset % 4, 0);
            assert_eq!(layout.save_offset + layout.save_len, layout.blob_len);
        }
    }

    #[test]
    fn the_layout_hashes_are_pinned() {
        // Recomputed by the FNV-1a walk over the fields above; a change to any field moves these and the PC side's pin.
        assert_eq!(ARMV7.hash(), PINNED_ARMV7_LAYOUT_HASH);
        assert_eq!(AARCH64.hash(), PINNED_AARCH64_LAYOUT_HASH);
    }

    const PINNED_ARMV7_LAYOUT_HASH: u64 = 395_945_050_492_294_272;
    const PINNED_AARCH64_LAYOUT_HASH: u64 = 7_647_534_792_890_474_860;
}
