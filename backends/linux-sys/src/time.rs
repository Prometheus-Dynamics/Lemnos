//! Clock readings for stamping device readings and GPIO edge events.

use std::io;

/// `CLOCK_MONOTONIC` now: the clock of V4L2 buffer and event timestamps, so the
/// time since a frame started is `monotonic_now() - timestamp`. An error if the
/// clock cannot be read; no other value stands in for it.
pub fn monotonic_now() -> io::Result<std::time::Duration> {
    clock_ns(libc::CLOCK_MONOTONIC).map(std::time::Duration::from_nanos)
}

/// Microseconds on `CLOCK_BOOTTIME`: monotonic like `CLOCK_MONOTONIC`, but it
/// keeps counting through suspend. It does not reset when a service restarts,
/// only at a reboot. An error if the boot clock cannot be read: the monotonic
/// clock stops during suspend, so it is never used in its place.
pub fn boottime_us() -> io::Result<u64> {
    clock_us(libc::CLOCK_BOOTTIME)
}

/// Nanoseconds on `CLOCK_BOOTTIME`, the clock of [`boottime_us`] (same error).
pub fn boottime_ns() -> io::Result<u64> {
    clock_ns(libc::CLOCK_BOOTTIME)
}

/// Converts a `CLOCK_MONOTONIC` instant (nanoseconds, as the GPIO uAPI v2
/// stamps its edge events) to microseconds on `CLOCK_BOOTTIME`, the clock of
/// [`boottime_us`]. Both clocks are read back to back here, so the offset
/// between them is current; it only moves when the system suspends. An error if
/// either clock cannot be read.
pub fn boottime_us_of_monotonic_ns(monotonic_ns: u64) -> io::Result<u64> {
    boottime_us_of_monotonic_ns_on(monotonic_ns, libc::CLOCK_BOOTTIME, libc::CLOCK_MONOTONIC)
}

fn boottime_us_of_monotonic_ns_on(
    monotonic_ns: u64,
    boot_clock: libc::clockid_t,
    mono_clock: libc::clockid_t,
) -> io::Result<u64> {
    let boot_now = clock_ns(boot_clock)?;
    let mono_now = clock_ns(mono_clock)?;
    Ok(boottime_us_for_monotonic(monotonic_ns, boot_now, mono_now))
}

/// The pure conversion behind [`boottime_us_of_monotonic_ns`]: `boot_now_ns` and
/// `mono_now_ns` are the two clocks read at the same instant. A stamp taken
/// before that instant maps to an earlier boot time. The result saturates at
/// zero rather than going negative.
pub fn boottime_us_for_monotonic(monotonic_ns: u64, boot_now_ns: u64, mono_now_ns: u64) -> u64 {
    let boot_ns = i128::from(monotonic_ns) + i128::from(boot_now_ns) - i128::from(mono_now_ns);
    u64::try_from(boot_ns.max(0) / 1_000).unwrap_or(u64::MAX)
}

fn clock_ns(clock: libc::clockid_t) -> io::Result<u64> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a live, writable `timespec`, which `clock_gettime`
    // fills in and nothing else touches.
    let r = unsafe { libc::clock_gettime(clock, &raw mut ts) };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    let out_of_range = || io::Error::new(io::ErrorKind::InvalidData, "clock reading out of range");
    let secs = u64::try_from(ts.tv_sec).map_err(|_| out_of_range())?;
    let nanos = u64::try_from(ts.tv_nsec).map_err(|_| out_of_range())?;
    secs.checked_mul(1_000_000_000)
        .and_then(|n| n.checked_add(nanos))
        .ok_or_else(out_of_range)
}

fn clock_us(clock: libc::clockid_t) -> io::Result<u64> {
    clock_ns(clock).map(|ns| ns / 1_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_moves_forward() {
        let a = monotonic_now().expect("monotonic clock");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = monotonic_now().expect("monotonic clock");
        assert!(b > a && b - a >= std::time::Duration::from_millis(2));
    }

    #[test]
    fn boot_clock_does_not_go_backwards() {
        let a = boottime_us().expect("boot clock");
        let b = boottime_us().expect("boot clock");
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
        let before = boottime_ns().expect("boot clock");
        let mono = clock_ns(libc::CLOCK_MONOTONIC).expect("monotonic clock");
        let call_start = boottime_ns().expect("boot clock");
        let converted_us = boottime_us_of_monotonic_ns(mono).expect("clocks");
        let call_end = boottime_ns().expect("boot clock");
        let after = boottime_ns().expect("boot clock");
        // The conversion reads the boot clock and then the monotonic clock, so the offset it
        // applies is short by the gap between those two reads, which is inside the call. The
        // result is also cut to microseconds. Both are bounded here by what was measured
        // around the call: a stamp taken after `before` cannot map to a boot time more than
        // that gap and one microsecond below it.
        let gap_ns = call_end - call_start;
        let converted_ns = converted_us * 1_000;
        assert!(
            converted_ns + gap_ns + 1_000 > before,
            "{converted_ns} + {gap_ns} <= {before}"
        );
        assert!(converted_ns <= after, "{converted_ns} > {after}");
    }

    /// A clock id the kernel rejects (`EINVAL`), standing for a clock that cannot be read.
    const BAD_CLOCK: libc::clockid_t = 9999;

    #[test]
    fn unreadable_clock_is_an_error_not_a_substitute() {
        let err = clock_ns(BAD_CLOCK).expect_err("bad clock id must fail");
        assert_eq!(err.raw_os_error(), Some(libc::EINVAL));
        assert!(clock_us(BAD_CLOCK).is_err());
    }

    #[test]
    fn conversion_fails_when_either_clock_is_unreadable() {
        let mono = clock_ns(libc::CLOCK_MONOTONIC).expect("monotonic clock");
        assert!(boottime_us_of_monotonic_ns_on(mono, BAD_CLOCK, libc::CLOCK_MONOTONIC).is_err());
        assert!(boottime_us_of_monotonic_ns_on(mono, libc::CLOCK_BOOTTIME, BAD_CLOCK).is_err());
        assert!(
            boottime_us_of_monotonic_ns_on(mono, libc::CLOCK_BOOTTIME, libc::CLOCK_MONOTONIC)
                .is_ok()
        );
    }
}
