//! FNV-1a, the function the layout hash folds with. The same function as `lemnos-flash-wire`'s
//! `hash` module, carried here so this crate has no dependencies; the vectors below are that module's.

/// The FNV-1a offset basis: where every walk starts.
pub const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// Folds one byte into `hash`.
pub const fn byte(hash: u64, value: u8) -> u64 {
    (hash ^ value as u64).wrapping_mul(0x0000_0100_0000_01b3)
}

/// Folds a `u64`, little-endian, into `hash`.
pub const fn word(mut hash: u64, value: u64) -> u64 {
    let mut i = 0;
    while i < 8 {
        hash = byte(hash, (value >> (8 * i)) as u8);
        i += 1;
    }
    hash
}

/// Folds a byte string into `hash`, its length first.
pub const fn bytes(hash: u64, value: &[u8]) -> u64 {
    let mut hash = word(hash, value.len() as u64);
    let mut i = 0;
    while i < value.len() {
        hash = byte(hash, value[i]);
        i += 1;
    }
    hash
}

/// Folds a string into `hash`, its length first.
pub const fn text(hash: u64, value: &str) -> u64 {
    bytes(hash, value.as_bytes())
}

/// The hash of a blob's bytes, as the PC side pins it.
pub const fn blob(value: &[u8]) -> u64 {
    bytes(OFFSET, value)
}

/// Test vectors shared with `lemnos-flash-wire`'s hash: `(what, value)`.
pub const VECTORS: [(&str, u64); 3] = [
    ("word(OFFSET, 1)", 0x89CD_3129_1D2A_EFA4),
    ("text(OFFSET, \"abc\")", 0xC11A_B6D2_519B_C2B2),
    ("word(OFFSET, u64::MAX)", 0x8CF5_1A8B_FCA3_883D),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vectors_are_the_fnv_walk_over_the_same_bytes() {
        assert_eq!(word(OFFSET, 1), VECTORS[0].1);
        assert_eq!(text(OFFSET, "abc"), VECTORS[1].1);
        assert_eq!(word(OFFSET, u64::MAX), VECTORS[2].1);
    }

    #[test]
    fn the_blob_hash_is_the_bytes_walk_with_the_length_first() {
        // The length of "abc" is folded first, so the blob hash of those three bytes is `text` of them.
        assert_eq!(blob(b"abc"), text(OFFSET, "abc"));
    }
}
