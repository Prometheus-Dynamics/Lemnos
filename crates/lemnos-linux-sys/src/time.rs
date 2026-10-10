//! Clock readings for stamping device readings.

/// Microseconds on `CLOCK_BOOTTIME`: monotonic like `CLOCK_MONOTONIC`, but it
/// keeps counting through suspend. It does not reset when a service restarts,
/// only at a reboot. Falls back to `CLOCK_MONOTONIC` if the boot clock cannot
/// be read, and to 0 if neither can.
pub fn boottime_us() -> u64 {
    clock_us(libc::CLOCK_BOOTTIME)
        .or_else(|| clock_us(libc::CLOCK_MONOTONIC))
        .unwrap_or(0)
}

fn clock_us(clock: libc::clockid_t) -> Option<u64> {
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
    Some(secs * 1_000_000 + nanos / 1_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_clock_does_not_go_backwards() {
        let a = boottime_us();
        let b = boottime_us();
        assert!(a > 0);
        assert!(b >= a);
    }
}
