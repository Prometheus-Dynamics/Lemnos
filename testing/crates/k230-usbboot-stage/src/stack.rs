//! The card image's stack discipline. The image's stack reserve sits BELOW its code (`image.ld`), with a guard band
//! of canary words under it. The entry paints the guard band with [`CANARY`] and the reserve with [`PATTERN`] before the
//! first call. A check point reads the guard band: a changed word means the stack has grown out of its reserve, and the
//! image takes the reset. The high-water mark is the scan of the reserve from the bottom up for the first word that is
//! no longer the pattern.
//!
//! The functions take the words as a reader (`index -> value`), so the image reads its reserve with volatile loads and
//! this module holds no pointer. The paint itself is in the entry's assembly, before any call.

/// What the entry paints the reserve with.
pub const PATTERN: u32 = 0xa5a5_a5a5;
/// What the entry paints the guard band with.
pub const CANARY: u32 = 0x5ca1_ab1e;

/// Whether every word of the guard band (`words` words, read by `read`) still holds [`CANARY`].
pub fn guard_ok(words: usize, read: impl Fn(usize) -> u32) -> bool {
    (0..words).all(|i| read(i) == CANARY)
}

/// The high-water mark in bytes: from the first changed word of a reserve of `words` words, up to the top. A reserve
/// nobody wrote has a mark of zero.
pub fn used_bytes(words: usize, read: impl Fn(usize) -> u32) -> usize {
    (0..words)
        .find(|&i| read(i) != PATTERN)
        .map_or(0, |i| (words - i) * 4)
}

/// What the entry paints the span below the guard band with (the RAM below the image, which no code of the image uses).
pub const BELOW_PATTERN: u32 = 0x3c3c_3c3c;

/// The span of words that do not hold [`BELOW_PATTERN`], as the first changed word and how many words changed, or
/// `None` when the span is unchanged. `words` is the span's length.
pub fn changed_span(words: usize, read: impl Fn(usize) -> u32) -> Option<(usize, usize)> {
    let first = (0..words).find(|&i| read(i) != BELOW_PATTERN)?;
    let count = (first..words).filter(|&i| read(i) != BELOW_PATTERN).count();
    Some((first, count))
}

/// Writes `v` as eight hex digits into `line` from `at`, and returns the end.
#[inline(never)]
pub fn put_hex32(line: &mut [u8], at: usize, v: u32) -> usize {
    for k in 0..8 {
        let nibble = (v >> (28 - 4 * k)) & 0xf;
        line[at + k] = b"0123456789abcdef"[nibble as usize];
    }
    at + 8
}

/// Writes `n` in decimal into `line` from `at`, and returns the end. Out of line, so one copy serves the image.
/// The line must have room for ten digits past `at`: a `u32` has at most ten.
#[inline(never)]
pub fn put_decimal(line: &mut [u8], at: usize, n: u32) -> usize {
    let mut p = 1u32;
    while n / p >= 10 {
        p *= 10;
    }
    let mut end = at;
    loop {
        line[end] = b'0' + (n / p % 10) as u8;
        end += 1;
        if p == 1 {
            return end;
        }
        p /= 10;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reserve of `words` words, painted, as the entry leaves it.
    fn painted(words: usize) -> Vec<u32> {
        vec![PATTERN; words]
    }

    #[test]
    fn an_untouched_reserve_has_a_mark_of_zero() {
        let v = painted(64);
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
    fn a_guard_band_is_intact_only_while_every_word_is_the_canary() {
        let mut g = vec![CANARY; 64];
        assert!(guard_ok(g.len(), |i| g[i]));
        // A change in the lowest word and one in the top word are both overflows.
        g[0] = PATTERN;
        assert!(!guard_ok(g.len(), |i| g[i]));
        g[0] = CANARY;
        g[63] = 0;
        assert!(!guard_ok(g.len(), |i| g[i]));
    }

    #[test]
    fn the_span_below_the_guard_is_unchanged_until_a_word_of_it_changes() {
        let mut v = vec![BELOW_PATTERN; 100];
        assert_eq!(changed_span(v.len(), |i| v[i]), None);
        v[37] = 0;
        assert_eq!(changed_span(v.len(), |i| v[i]), Some((37, 1)));
    }

    #[test]
    fn the_span_reports_the_first_changed_word_and_how_many_changed() {
        let mut v = vec![BELOW_PATTERN; 100];
        v[90] = 1;
        v[12] = 2;
        v[13] = 3;
        // The first changed word is the lowest address, and the count covers every changed word, not just the first run.
        assert_eq!(changed_span(v.len(), |i| v[i]), Some((12, 3)));
        let mut last = [BELOW_PATTERN; 4];
        last[3] = 0;
        assert_eq!(changed_span(last.len(), |i| last[i]), Some((3, 1)));
    }

    #[test]
    fn put_hex32_writes_the_address_in_eight_digits() {
        let mut line = [b'#'; 12];
        let end = put_hex32(&mut line, 2, 0x8035_bf00);
        assert_eq!(&line[2..end], b"8035bf00");
        assert_eq!(line[1], b'#');
    }

    #[test]
    fn put_decimal_writes_the_number_in_base_ten() {
        for (n, want) in [
            (0u32, "0"),
            (7, "7"),
            (16384, "16384"),
            (4_294_967_295, "4294967295"),
        ] {
            let mut line = [b'#'; 12];
            let end = put_decimal(&mut line, 2, n);
            assert_eq!(&line[2..end], want.as_bytes());
            assert_eq!(line[1], b'#', "nothing before the start is written");
        }
    }
}
