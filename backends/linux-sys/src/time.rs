//! The clock Lemnos stamps with on Linux: `CLOCK_MONOTONIC`.
//!
//! Every time Lemnos hands out on Linux is nanoseconds on this clock: a stream frame's
//! timestamp, a subscribed value's stamp, an interrupt's or an edge's time, and the `now_ns` a
//! driver's tick is given. It is the clock of the kernel's `ktime_get_ns()`, of V4L2 buffers
//! flagged `V4L2_BUF_FLAG_TIMESTAMP_MONOTONIC` and of GPIO uAPI v2 edge events, so a time from
//! any of them compares directly with [`monotonic_ns`].

use std::io;

/// `CLOCK_MONOTONIC` now, in nanoseconds. An error if the clock cannot be read; no other clock
/// stands in for it.
pub fn monotonic_ns() -> io::Result<u64> {
    clock_ns(libc::CLOCK_MONOTONIC)
}

/// `CLOCK_MONOTONIC` now, so the time since a frame started is `monotonic_now() - timestamp`.
/// The same reading and error as [`monotonic_ns`].
pub fn monotonic_now() -> io::Result<std::time::Duration> {
    monotonic_ns().map(std::time::Duration::from_nanos)
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
    fn it_reads_clock_monotonic() {
        // Bracketed by the libc clock itself: a reading of any other clock (the boot clock
        // after a suspend, the wall clock) lands outside.
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let raw = |ts: &libc::timespec| ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64;
        // SAFETY: `ts` is a live, writable `timespec`.
        let r = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut ts) };
        assert_eq!(r, 0);
        let before = raw(&ts);
        let now = monotonic_ns().expect("monotonic clock");
        // SAFETY: as above.
        let r = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut ts) };
        assert_eq!(r, 0);
        let after = raw(&ts);
        assert!(
            before <= now && now <= after,
            "{before} <= {now} <= {after}"
        );
    }

    /// A clock id the kernel rejects (`EINVAL`), standing for a clock that cannot be read.
    const BAD_CLOCK: libc::clockid_t = 9999;

    #[test]
    fn unreadable_clock_is_an_error_not_a_substitute() {
        let err = clock_ns(BAD_CLOCK).expect_err("bad clock id must fail");
        assert_eq!(err.raw_os_error(), Some(libc::EINVAL));
    }
}
