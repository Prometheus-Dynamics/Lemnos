//! The time a sample belongs to.
//!
//! [`SampleTime`] is nanoseconds of `CLOCK_MONOTONIC`: the clock that starts at boot, never jumps
//! and never goes backwards (Linux's monotonic clock does not count time suspended). A frame
//! and everything computed from it carry the same value, which is what makes "the value at or
//! before this frame" a lookup.
//!
//! Every process on one machine reads the same `CLOCK_MONOTONIC`, so times written by one
//! process compare directly with times read by another. A time from another machine must be
//! moved onto this machine's clock first; nothing here does that.
//!
//! No `std` and no allocation. [`SampleTime::now`] exists on Unix targets, where it reads the
//! clock with `clock_gettime`.

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

    /// The current time: `CLOCK_MONOTONIC` read now.
    ///
    /// `clock_gettime` on this clock is served from the vDSO on Linux, so it is normally not a
    /// system call. If the clock can't be read, which Linux never does for this clock, the
    /// result is [`SampleTime::ZERO`].
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
