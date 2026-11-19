//! The card image's stack discipline. The image reserves a stack below its own footprint (`image.ld`), and the entry
//! paints the whole reserve with [`PATTERN`] before the first call, with [`CANARY`] in its lowest word. A check point
//! then reads the canary (a changed canary is an overflow) and scans up from the bottom for the first word that is no
//! longer the pattern: that gives the high-water mark, the deepest the stack has gone.
//!
//! The functions take the words as a reader (`index -> value`), so the image reads its reserve with volatile loads and
//! this module holds no pointer. The paint itself is in the entry's assembly, before any call.

/// What the entry paints the reserve with.
pub const PATTERN: u32 = 0xa5a5_a5a5;
/// The lowest word of the reserve, written after the paint.
pub const CANARY: u32 = 0x5ca1_ab1e;

/// Whether the canary (word 0) still holds [`CANARY`].
pub fn canary_ok(read: impl Fn(usize) -> u32) -> bool {
    read(0) == CANARY
}

/// The high-water mark in bytes: from the first changed word of a reserve of `words` words, up to the top. A reserve
/// nobody wrote has a mark of zero. The canary (word 0) is not counted.
pub fn used_bytes(words: usize, read: impl Fn(usize) -> u32) -> usize {
    (1..words)
        .find(|&i| read(i) != PATTERN)
        .map_or(0, |i| (words - i) * 4)
}

/// `n` in decimal: the digits, left-aligned in a ten-byte buffer, and how many there are.
pub fn decimal(mut n: usize) -> ([u8; 20], usize) {
    let mut digits = [0u8; 20];
    let mut len = 0;
    loop {
        digits[len] = b'0' + (n % 10) as u8;
        len += 1;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    digits[..len].reverse();
    (digits, len)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reserve of `words` words, painted, as the entry leaves it.
    fn painted(words: usize) -> Vec<u32> {
        let mut v = vec![PATTERN; words];
        v[0] = CANARY;
        v
    }

    #[test]
    fn an_untouched_reserve_has_a_mark_of_zero_and_an_intact_canary() {
        let v = painted(64);
        assert!(canary_ok(|i| v[i]));
        assert_eq!(used_bytes(v.len(), |i| v[i]), 0);
    }

    #[test]
    fn the_mark_is_the_depth_from_the_top_to_the_lowest_word_written() {
        let mut v = painted(64);
        // Written 10 words above the bottom: the stack went down to word 10, so 54 words of 64 are used.
        v[10] = 0x1234;
        v[40] = 0x5678;
        assert_eq!(used_bytes(v.len(), |i| v[i]), (64 - 10) * 4);
        // Written at the very top: one word used.
        let mut w = painted(64);
        w[63] = 0;
        assert_eq!(used_bytes(w.len(), |i| w[i]), 4);
    }

    #[test]
    fn a_changed_canary_is_an_overflow() {
        let mut v = painted(8);
        v[0] = PATTERN;
        assert!(!canary_ok(|i| v[i]));
    }

    #[test]
    fn decimal_text_is_the_number_in_base_ten() {
        for (n, want) in [
            (0usize, "0"),
            (7, "7"),
            (16384, "16384"),
            (1_000_000, "1000000"),
        ] {
            let (digits, len) = decimal(n);
            assert_eq!(core::str::from_utf8(&digits[..len]).unwrap(), want);
        }
    }
}
