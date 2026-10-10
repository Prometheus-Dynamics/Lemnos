//! Waiting for descriptors to become ready.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};
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
    let timeout_ms = timeout_ms(timeout);
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

/// One descriptor in a [`poll_many`] set; the same layout as `struct
/// pollfd`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollFd {
    fd: RawFd,
    events: i16,
    revents: i16,
}

impl PollFd {
    /// Waits for `events` on `fd`. The descriptor must stay open until the
    /// [`poll_many`] call that uses this entry returns.
    pub fn new(fd: BorrowedFd<'_>, events: i16) -> Self {
        Self {
            fd: fd.as_raw_fd(),
            events,
            revents: 0,
        }
    }

    /// Waits for `events` on a raw descriptor (one a caller holds through
    /// another handle). Safe: `poll` reports a closed descriptor as
    /// `POLLNVAL` instead of touching it.
    pub fn from_raw(fd: RawFd, events: i16) -> Self {
        Self {
            fd,
            events,
            revents: 0,
        }
    }

    /// The events the last [`poll_many`] reported.
    pub fn revents(&self) -> i16 {
        self.revents
    }
}

fn timespec(d: Duration) -> libc::timespec {
    libc::timespec {
        tv_sec: libc::time_t::try_from(d.as_secs()).unwrap_or(libc::time_t::MAX),
        // Below one second, so it fits a `c_long` on every 64-bit target.
        tv_nsec: d.subsec_nanos() as libc::c_long,
    }
}

fn timeout_ms(timeout: Option<Duration>) -> i32 {
    match timeout {
        None => -1,
        // Round up so a short positive timeout does not become a busy poll.
        Some(d) => i32::try_from(d.as_nanos().div_ceil(1_000_000)).unwrap_or(i32::MAX),
    }
}

/// Waits until any of `fds` reports one of its events or `timeout` passes
/// (`None`: forever). Returns how many entries have events (0 on timeout);
/// each entry's [`PollFd::revents`] says which. Returns 0 on `EINTR`, so a
/// caller's loop sees signals promptly.
pub fn poll_many(fds: &mut [PollFd], timeout: Option<Duration>) -> io::Result<usize> {
    let count = libc::nfds_t::try_from(fds.len())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    for fd in fds.iter_mut() {
        fd.revents = 0;
    }
    // `ppoll` takes the timeout in nanoseconds (`poll` only in milliseconds,
    // which would round a 10 ms schedule to whole milliseconds).
    let spec = timeout.map(timespec);
    let spec_ptr = spec.as_ref().map_or(std::ptr::null(), std::ptr::from_ref);
    // SAFETY: `PollFd` is `repr(C)` with the fields of `struct pollfd` in
    // order (checked by `poll_fd_matches_the_kernel_layout`), and `fds` is a
    // live, writable slice of `count` entries. `spec_ptr` is null (wait
    // forever) or points at `spec`, which lives across the call; a null
    // signal mask keeps the current one.
    let r = unsafe {
        libc::ppoll(
            fds.as_mut_ptr().cast::<libc::pollfd>(),
            count,
            spec_ptr,
            std::ptr::null(),
        )
    };
    if r >= 0 {
        return Ok(r as usize);
    }
    let e = io::Error::last_os_error();
    if e.kind() == io::ErrorKind::Interrupted {
        Ok(0)
    } else {
        Err(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsFd;

    #[test]
    fn many_waits_for_sub_millisecond_timeouts() {
        let (reader, _writer) = std::io::pipe().unwrap();
        let mut fds = [PollFd::new(reader.as_fd(), POLLIN)];
        let start = std::time::Instant::now();
        assert_eq!(
            poll_many(&mut fds, Some(Duration::from_micros(500))).unwrap(),
            0
        );
        let waited = start.elapsed();
        assert!(waited >= Duration::from_micros(500), "{waited:?}");
        assert!(waited < Duration::from_millis(50), "{waited:?}");
    }

    #[test]
    fn reports_readiness_and_timeouts() {
        let (reader, mut writer) = std::io::pipe().unwrap();
        let idle = poll_one(reader.as_fd(), POLLIN, Some(Duration::from_millis(1))).unwrap();
        assert_eq!(idle, 0);
        writer.write_all(b"x").unwrap();
        let ready = poll_one(reader.as_fd(), POLLIN, Some(Duration::ZERO)).unwrap();
        assert_eq!(ready & POLLIN, POLLIN);
    }

    #[test]
    fn poll_fd_matches_the_kernel_layout() {
        assert_eq!(
            std::mem::size_of::<PollFd>(),
            std::mem::size_of::<libc::pollfd>()
        );
        assert_eq!(
            std::mem::align_of::<PollFd>(),
            std::mem::align_of::<libc::pollfd>()
        );
        assert_eq!(
            std::mem::offset_of!(PollFd, events),
            std::mem::offset_of!(libc::pollfd, events)
        );
        assert_eq!(
            std::mem::offset_of!(PollFd, revents),
            std::mem::offset_of!(libc::pollfd, revents)
        );
    }

    #[test]
    fn poll_many_reports_the_ready_entry() {
        let (idle, _keep) = std::io::pipe().unwrap();
        let (ready, mut writer) = std::io::pipe().unwrap();
        writer.write_all(b"x").unwrap();
        let mut fds = [
            PollFd::new(idle.as_fd(), POLLIN),
            PollFd::new(ready.as_fd(), POLLIN),
        ];
        assert_eq!(poll_many(&mut fds, Some(Duration::ZERO)).unwrap(), 1);
        assert_eq!(fds[0].revents(), 0);
        assert_eq!(fds[1].revents() & POLLIN, POLLIN);
    }
}
