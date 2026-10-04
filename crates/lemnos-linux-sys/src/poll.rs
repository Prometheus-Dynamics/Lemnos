//! Waiting for one descriptor to become ready.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::time::Duration;

/// `POLLIN`.
pub const POLLIN: i16 = libc::POLLIN;
/// `POLLOUT`.
pub const POLLOUT: i16 = libc::POLLOUT;
/// `POLLPRI`.
pub const POLLPRI: i16 = libc::POLLPRI;
/// `POLLERR`.
pub const POLLERR: i16 = libc::POLLERR;
/// `POLLHUP`.
pub const POLLHUP: i16 = libc::POLLHUP;

/// Waits until `fd` reports one of `events` or `timeout` passes (`None`:
/// forever). Returns the reported events (0 on timeout). Retries on `EINTR`
/// (restarting the full timeout).
pub fn poll_one(fd: BorrowedFd<'_>, events: i16, timeout: Option<Duration>) -> io::Result<i16> {
    let timeout_ms = match timeout {
        None => -1,
        // Round up so a short positive timeout does not become a busy poll.
        Some(d) => i32::try_from(d.as_nanos().div_ceil(1_000_000)).unwrap_or(i32::MAX),
    };
    loop {
        let mut pfd = libc::pollfd {
            fd: fd.as_raw_fd(),
            events,
            revents: 0,
        };
        // SAFETY: `pfd` is one live, writable `pollfd`, matching the count 1.
        let r = unsafe { libc::poll(&raw mut pfd, 1, timeout_ms) };
        if r >= 0 {
            return Ok(pfd.revents);
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsFd;

    #[test]
    fn reports_readiness_and_timeouts() {
        let (reader, mut writer) = std::io::pipe().unwrap();
        let idle = poll_one(reader.as_fd(), POLLIN, Some(Duration::from_millis(1))).unwrap();
        assert_eq!(idle, 0);
        writer.write_all(b"x").unwrap();
        let ready = poll_one(reader.as_fd(), POLLIN, Some(Duration::ZERO)).unwrap();
        assert_eq!(ready & POLLIN, POLLIN);
    }
}
