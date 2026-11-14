//! The least time a long install or flash may be given: `deadline_ms >= 2 x bytes / R + F`.
//!
//! The one place the rule's two constants live, for the `system-slots` and `system-flash` drivers.
//! **Both constants are assumptions.** No board run has measured an install's or a flash's rate or
//! its fixed terms (`docs/daemon.md`, "The install deadline (the sizing rule)"); a board run that
//! does replaces them here and nowhere else.
//!
//! A job started with a deadline below the floor for its size is refused at its start, by name,
//! and nothing is written. A job started with no deadline has no floor.

/// R: the worst sustained rate of the target, in bytes per second. **Assumed, not measured.**
pub const ASSUMED_RATE_BYTES_PER_S: u64 = 1_000_000;
/// F: the fixed terms of an install or flash (manifest and signature check, set-up, syncs, the
/// swap into the slot), in milliseconds. **Assumed, not measured.**
pub const ASSUMED_FIXED_MS: u64 = 60_000;

/// The least `deadline_ms` for `bytes` of image: one write and one read-back of every byte at
/// [`ASSUMED_RATE_BYTES_PER_S`], plus [`ASSUMED_FIXED_MS`], rounded up to a whole millisecond.
#[must_use]
pub fn floor_ms(bytes: u64) -> u64 {
    let moved = u128::from(bytes) * 2 * 1000;
    let rate = u128::from(ASSUMED_RATE_BYTES_PER_S);
    let ms = moved.div_ceil(rate) + u128::from(ASSUMED_FIXED_MS);
    u64::try_from(ms).unwrap_or(u64::MAX)
}

/// The refusal for a `what` ("install" or "flash") of `bytes` started with `deadline_ms`, or
/// `None` when there is no deadline (no floor) or it reaches the floor.
#[must_use]
pub fn refusal(what: &str, bytes: u64, deadline_ms: Option<u64>) -> Option<String> {
    let given = deadline_ms?;
    let needed = floor_ms(bytes);
    (given < needed).then(|| {
        format!(
            "this {what} needs at least {needed} ms for {bytes} bytes; the deadline given is {given}"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_floor_is_the_rule_rounded_up() {
        assert_eq!(floor_ms(0), 60_000);
        assert_eq!(floor_ms(1_000_000), 62_000);
        // 2 x 68,890,907 B at 1 MB/s is 137,781.814 s of ms: 137,782 ms, plus 60,000.
        assert_eq!(floor_ms(68_890_907), 197_782);
    }

    #[test]
    fn below_the_floor_is_refused_by_name_and_no_deadline_has_none() {
        let text = refusal("install", 1_000_000, Some(61_999)).expect("refused");
        assert_eq!(
            text,
            "this install needs at least 62000 ms for 1000000 bytes; the deadline given is 61999"
        );
        assert_eq!(refusal("install", 1_000_000, Some(62_000)), None);
        assert_eq!(refusal("install", 1_000_000, None), None);
    }
}
