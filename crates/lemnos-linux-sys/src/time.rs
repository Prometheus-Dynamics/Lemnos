//! Clock readings for stamping device readings and GPIO edge events.

/// `CLOCK_MONOTONIC` now: the clock of V4L2 buffer and event timestamps, so the
/// time since a frame started is `monotonic_now() - timestamp`. Zero if the clock
/// cannot be read.
pub fn monotonic_now() -> std::time::Duration {
    clock_ns(libc::CLOCK_MONOTONIC)
        .map_or(std::time::Duration::ZERO, std::time::Duration::from_nanos)
}

/// Microseconds on `CLOCK_BOOTTIME`: monotonic like `CLOCK_MONOTONIC`, but it
/// keeps counting through suspend. It does not reset when a service restarts,
/// only at a reboot. Falls back to `CLOCK_MONOTONIC` if the boot clock cannot
/// be read, and to 0 if neither can.
pub fn boottime_us() -> u64 {
    clock_us(libc::CLOCK_BOOTTIME)
        .or_else(|| clock_us(libc::CLOCK_MONOTONIC))
        .unwrap_or(0)
}

/// Converts a `CLOCK_MONOTONIC` instant (nanoseconds, as the GPIO uAPI v2
/// stamps its edge events) to microseconds on `CLOCK_BOOTTIME`, the clock of
/// [`boottime_us`]. Both clocks are read back to back here, so the offset
/// between them is current; it only moves when the system suspends.
pub fn boottime_us_of_monotonic_ns(monotonic_ns: u64) -> u64 {
    match (
        clock_ns(libc::CLOCK_BOOTTIME),
        clock_ns(libc::CLOCK_MONOTONIC),
    ) {
        (Some(boot_now), Some(mono_now)) => {
            boottime_us_for_monotonic(monotonic_ns, boot_now, mono_now)
        }
        _ => monotonic_ns / 1_000,
    }
}

/// The pure conversion behind [`boottime_us_of_monotonic_ns`]: `boot_now_ns` and
/// `mono_now_ns` are the two clocks read at the same instant. A stamp taken
/// before that instant maps to an earlier boot time. The result saturates at
/// zero rather than going negative.
pub fn boottime_us_for_monotonic(monotonic_ns: u64, boot_now_ns: u64, mono_now_ns: u64) -> u64 {
    let boot_ns = i128::from(monotonic_ns) + i128::from(boot_now_ns) - i128::from(mono_now_ns);
    u64::try_from(boot_ns.max(0) / 1_000).unwrap_or(u64::MAX)
}

fn clock_ns(clock: libc::clockid_t) -> Option<u64> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a live, writable `timespec`, which `clock_gettime`
    // fills in and nothing else touches.
    let r = unsafe { libc::clock_gettime(clock, &raw mut ts) };
    if r != 0 {
        return None;
    }
    let secs = u64::try_from(ts.tv_sec).ok()?;
    let nanos = u64::try_from(ts.tv_nsec).ok()?;
    secs.checked_mul(1_000_000_000)?.checked_add(nanos)
}

fn clock_us(clock: libc::clockid_t) -> Option<u64> {
    clock_ns(clock).map(|ns| ns / 1_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_moves_forward() {
        let a = monotonic_now();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = monotonic_now();
        assert!(b > a && b - a >= std::time::Duration::from_millis(2));
    }

    #[test]
    fn boot_clock_does_not_go_backwards() {
        let a = boottime_us();
        let b = boottime_us();
        assert!(a > 0);
        assert!(b >= a);
    }

    #[test]
    fn monotonic_stamps_convert_onto_the_boot_clock() {
        // Boot clock 4 s ahead of the monotonic clock (a 4 s suspend before the
        // service started, say): an edge at monotonic 999.5 ms is at boot 4.9995 s.
        assert_eq!(
            boottime_us_for_monotonic(999_500_000, 5_000_000_000, 1_000_000_000),
            4_999_500
        );
        // With the two clocks equal, the stamp maps to itself.
        assert_eq!(
            boottime_us_for_monotonic(7_250_000, 7_000_000, 7_000_000),
            7_250
        );
        // A stamp before the reference instant still converts (earlier boot time).
        assert_eq!(
            boottime_us_for_monotonic(1_000_000_000, 3_000_000_000, 2_000_000_000),
            2_000_000
        );
    }

    #[test]
    fn monotonic_stamps_land_between_the_boot_clock_readings() {
        let before = boottime_us();
        let mono = clock_ns(libc::CLOCK_MONOTONIC).expect("monotonic clock");
        let converted = boottime_us_of_monotonic_ns(mono);
        let after = boottime_us();
        assert!(converted >= before, "{converted} < {before}");
        assert!(converted <= after, "{converted} > {after}");
    }
}
