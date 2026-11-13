//! FNV-1a, the function every layout hash of this crate folds with. It is written out here, not
//! taken from `lemnos-core`, because the ring is built without the protocol and so without core.
//! `lemnos-core`'s `layout` module uses the same function; a test in the protocol checks they agree.

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

/// Folds a string into `hash`, its length first.
pub const fn text(hash: u64, value: &str) -> u64 {
    let bytes = value.as_bytes();
    let mut hash = word(hash, bytes.len() as u64);
    let mut i = 0;
    while i < bytes.len() {
        hash = byte(hash, bytes[i]);
        i += 1;
    }
    hash
}
