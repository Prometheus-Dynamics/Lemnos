//! What a usbfs failure amounts to, and the kernel's timeout unit.

use std::io;
use std::time::Duration;

use lemnos_hal::ErrorKind;

/// What a failed usbfs call amounts to.
pub(crate) fn kind_of(error: &io::Error) -> ErrorKind {
    match error.raw_os_error() {
        Some(errno) => kind_of_errno(errno),
        // The layer below reports a completion that can't be true as `InvalidData`. That is the
        // kernel's or the device's doing, not a bad request.
        None if error.kind() == io::ErrorKind::InvalidData => ErrorKind::Failed,
        None => ErrorKind::from_io(error.kind()),
    }
}

/// What a usbfs errno amounts to. `ErrorKind` has no stall, so a stalled endpoint is `Failed`.
pub(crate) fn kind_of_errno(errno: i32) -> ErrorKind {
    match errno {
        libc::ETIME | libc::ETIMEDOUT => ErrorKind::Timeout,
        libc::ENODEV | libc::ESHUTDOWN | libc::ENXIO | libc::EHOSTUNREACH => ErrorKind::Unavailable,
        libc::EPIPE => ErrorKind::Failed,
        _ => ErrorKind::from_errno(errno),
    }
}

/// `timeout` in usbfs's milliseconds, rounded up, and never zero: usbfs reads zero as "forever".
pub(crate) fn timeout_ms(timeout: Duration) -> u32 {
    u32::try_from(timeout.as_nanos().div_ceil(1_000_000))
        .unwrap_or(u32::MAX)
        .max(1)
}
