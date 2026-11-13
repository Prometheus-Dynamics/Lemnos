//! The time a sample belongs to.
//!
//! [`SampleTime`] is nanoseconds of `CLOCK_MONOTONIC`: the clock that starts at boot, never jumps
//! and never goes backwards (Linux's monotonic clock does not count time suspended). A frame
//! and everything computed from it carry the same value, which is what makes "the value at or
//! before this frame" a lookup.
//!
//! Every process on one machine reads the same clock, so times written by one process compare
//! directly with times read by another. A time from another machine must be moved onto this
//! machine's clock first; nothing here does that, and times from different machines are not
//! comparable.
//!
//! No `std` and no allocation. [`SampleTime::now`] reads the clock on every platform that has
//! one: `CLOCK_MONOTONIC` on Unix (Linux and macOS, through `clock_gettime`), and
//! `QueryPerformanceCounter` on Windows. Both clocks are system-wide and never go backwards.
//! Only Linux's is the one the rest of Lemnos is documented against, and Linux keeps exactly the
//! same call it always made.

#![no_std]

use core::fmt;

/// A time in nanoseconds of `CLOCK_MONOTONIC`.
///
/// Stored and compared as a bare `u64`, which is also how a ring stores it. With the `serde`
/// feature it serializes as that `u64`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(transparent)
)]
pub struct SampleTime(u64);

impl SampleTime {
    /// The earliest time: the clock's zero, at boot.
    pub const ZERO: Self = Self(0);

    /// A time from nanoseconds of `CLOCK_MONOTONIC`.
    #[must_use]
    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    /// This time in nanoseconds of `CLOCK_MONOTONIC`.
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// Nanoseconds since `earlier`, or zero if `earlier` is later.
    #[must_use]
    pub const fn saturating_since(self, earlier: Self) -> u64 {
        self.0.saturating_sub(earlier.0)
    }

    /// The current time: the monotonic clock read now, in nanoseconds (see the crate docs). On
    /// Linux that is `CLOCK_MONOTONIC`, read through `clock_gettime`, which the vDSO serves, so it
    /// is normally not a system call. If the clock can't be read, which the platforms don't do for
    /// these clocks, the result is [`SampleTime::ZERO`]. Not comparable across machines.
    #[cfg(unix)]
    #[must_use]
    pub fn now() -> Self {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `time` is a valid, writable `timespec` for the call to fill.
        #[allow(unsafe_code)]
        let status = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut time) };
        if status != 0 {
            return Self::ZERO;
        }
        // Both are non-negative for this clock; a negative value would be a broken kernel.
        let seconds = u64::try_from(time.tv_sec).unwrap_or(0);
        let nanos = u64::try_from(time.tv_nsec).unwrap_or(0);
        Self(seconds.saturating_mul(1_000_000_000).saturating_add(nanos))
    }
}

/// Windows: `QueryPerformanceCounter`, scaled by `QueryPerformanceFrequency` to nanoseconds. Both
/// are declared here directly, so the crate needs no Windows dependency; they are the same
/// functions `std::time::Instant` uses, and the counter is system-wide.
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_clock {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn QueryPerformanceCounter(count: *mut i64) -> i32;
        fn QueryPerformanceFrequency(frequency: *mut i64) -> i32;
    }

    /// The counter now and its frequency, or `None` if the system won't say.
    pub(super) fn read() -> Option<(i64, i64)> {
        let mut count = 0_i64;
        let mut frequency = 0_i64;
        // SAFETY: `count` is a valid, writable `i64` for the call to fill.
        let counted = unsafe { QueryPerformanceCounter(&raw mut count) };
        // SAFETY: `frequency` is a valid, writable `i64` for the call to fill.
        let framed = unsafe { QueryPerformanceFrequency(&raw mut frequency) };
        (counted != 0 && framed != 0 && frequency > 0).then_some((count, frequency))
    }
}

#[cfg(windows)]
impl SampleTime {
    /// The current time, from `QueryPerformanceCounter`. See [`SampleTime::now`].
    #[must_use]
    pub fn now() -> Self {
        let Some((count, frequency)) = windows_clock::read() else {
            return Self::ZERO;
        };
        // The counter is non-negative for this clock. Scaled in 128 bits so that it cannot
        // overflow before the division.
        let nanos = u128::from(u64::try_from(count).unwrap_or(0)) * 1_000_000_000
            / u128::from(frequency.unsigned_abs());
        Self(u64::try_from(nanos).unwrap_or(u64::MAX))
    }
}

impl fmt::Display for SampleTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}ns", self.0)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::SampleTime;

    #[test]
    fn nanoseconds_round_trip_and_order() {
        let earlier = SampleTime::from_nanos(5);
        let later = SampleTime::from_nanos(12);
        assert_eq!(later.as_nanos(), 12);
        assert!(earlier < later);
        assert_eq!(later.saturating_since(earlier), 7);
        assert_eq!(earlier.saturating_since(later), 0);
        assert_eq!(SampleTime::ZERO.as_nanos(), 0);
        assert_eq!(std::format!("{later}"), "12ns");
    }

    /// `now` never goes backwards, on every platform that has a monotonic clock.
    #[cfg(any(unix, windows))]
    #[test]
    fn now_never_goes_backwards() {
        let mut last = SampleTime::now();
        for _ in 0..1000 {
            let next = SampleTime::now();
            assert!(last <= next, "{last} > {next}");
            last = next;
        }
        assert!(last.as_nanos() > 0, "the clock reads zero");
    }

    /// `now` reads `CLOCK_MONOTONIC`: it lands between two readings of that clock taken by
    /// another route, and never goes backwards.
    #[cfg(target_os = "linux")]
    #[test]
    fn now_reads_the_monotonic_clock() {
        fn monotonic_by_syscall() -> u64 {
            let mut time = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: the raw system call writes one `timespec` into `time`, which is valid.
            #[allow(unsafe_code)]
            let status = unsafe {
                libc::syscall(
                    libc::SYS_clock_gettime,
                    libc::CLOCK_MONOTONIC,
                    &raw mut time,
                )
            };
            assert_eq!(status, 0);
            time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64
        }
        let before = monotonic_by_syscall();
        let first = SampleTime::now();
        let second = SampleTime::now();
        let after = monotonic_by_syscall();
        assert!(before <= first.as_nanos(), "{before} > {first}");
        assert!(first <= second);
        assert!(second.as_nanos() <= after, "{second} > {after}");
    }
}
