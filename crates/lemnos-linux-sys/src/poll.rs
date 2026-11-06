//! Waiting for descriptors to become ready.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Duration;

/// `POLLIN`.
pub const POLLIN: i16 = libc::POLLIN;
/// `POLLOUT`.
pub const POLLOUT: i16 = libc::POLLOUT;
/// `POLLPRI`: priority data, such as a V4L2 event.
pub const POLLPRI: i16 = libc::POLLPRI;
/// `POLLERR`: an error on the descriptor (a failed fence, for one).
pub const POLLERR: i16 = libc::POLLERR;
/// `POLLHUP`: the peer went away.
pub const POLLHUP: i16 = libc::POLLHUP;
/// `POLLNVAL`: the descriptor is not open.
pub const POLLNVAL: i16 = libc::POLLNVAL;

/// Opens `path` read-only, non-blocking and close-on-exec, for a caller that
/// waits with [`poll_one`] and then reads what is ready: a read with nothing
/// buffered returns `WouldBlock` instead of sleeping.
pub fn open_nonblocking(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

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

// `libc` deprecates `time_t` and `suseconds_t` on musl (they will become 64-bit on 32-bit musl
// targets). The fields are those types, so the alias is named here either way.
#[allow(deprecated)]
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

/// Readiness of a device descriptor, as `poll(2)` reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ready {
    /// `POLLIN`: a capture buffer can be dequeued (or data read).
    pub readable: bool,
    /// `POLLOUT`: an output buffer can be dequeued.
    pub writable: bool,
    /// `POLLPRI`: a V4L2 event is pending, or a media request completed.
    pub priority: bool,
    /// `POLLERR`: the device reported an error (for example, streaming stopped).
    pub error: bool,
    /// `POLLHUP`: the device went away.
    pub hangup: bool,
}

impl Ready {
    /// True when nothing is ready (the wait timed out).
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The readiness in `revents`.
    pub fn from_revents(revents: i16) -> Self {
        Self {
            readable: revents & POLLIN != 0,
            writable: revents & POLLOUT != 0,
            priority: revents & POLLPRI != 0,
            error: revents & POLLERR != 0,
            hangup: revents & POLLHUP != 0,
        }
    }
}

/// What to wait for on one descriptor with [`poll_ready`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Wait {
    /// `POLLIN`.
    pub readable: bool,
    /// `POLLPRI` (V4L2 events).
    pub priority: bool,
}

impl Wait {
    /// Nothing (errors and hang-ups are still reported).
    pub const NONE: Wait = Wait {
        readable: false,
        priority: false,
    };
    /// `POLLIN`.
    pub const READABLE: Wait = Wait {
        readable: true,
        priority: false,
    };
    /// `POLLPRI`.
    pub const PRIORITY: Wait = Wait {
        readable: false,
        priority: true,
    };

    /// Both waits.
    pub fn union(self, other: Wait) -> Wait {
        Wait {
            readable: self.readable || other.readable,
            priority: self.priority || other.priority,
        }
    }

    /// The `poll` events of this wait.
    pub fn events(self) -> i16 {
        let mut e = 0;
        if self.readable {
            e |= POLLIN;
        }
        if self.priority {
            e |= POLLPRI;
        }
        e
    }
}

/// [`poll_many`], restarting on `EINTR` (with the full timeout again).
fn poll_restarting(fds: &mut [PollFd], timeout: Option<Duration>) -> io::Result<()> {
    let count = libc::nfds_t::try_from(fds.len())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let spec = timeout.map(timespec);
    let spec_ptr = spec.as_ref().map_or(std::ptr::null(), std::ptr::from_ref);
    loop {
        for fd in fds.iter_mut() {
            fd.revents = 0;
        }
        // SAFETY: as in `poll_many`: `PollFd` has the layout of `struct pollfd`,
        // `fds` is a live, writable slice of `count` entries, and `spec_ptr` is
        // null or points at `spec`.
        let r = unsafe {
            libc::ppoll(
                fds.as_mut_ptr().cast::<libc::pollfd>(),
                count,
                spec_ptr,
                std::ptr::null(),
            )
        };
        if r >= 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// [`poll_ready`] with the results written to `ready` (one per descriptor, in
/// order), without allocating for up to 8 descriptors: a frame's wait runs on
/// every frame.
///
/// # Panics
///
/// When `ready` is shorter than `fds`.
pub fn poll_ready_into(
    fds: &[(BorrowedFd<'_>, Wait)],
    timeout: Option<Duration>,
    ready: &mut [Ready],
) -> io::Result<()> {
    assert!(
        ready.len() >= fds.len(),
        "poll_ready_into: one result per descriptor"
    );
    const INLINE: usize = 8;
    let unset = PollFd::from_raw(-1, 0);
    let mut inline = [unset; INLINE];
    let mut spilled: Vec<PollFd> = Vec::new();
    let pfds: &mut [PollFd] = if fds.len() <= INLINE {
        &mut inline[..fds.len()]
    } else {
        spilled.resize(fds.len(), unset);
        &mut spilled
    };
    for (p, (fd, wait)) in pfds.iter_mut().zip(fds) {
        *p = PollFd::new(*fd, wait.events());
    }
    poll_restarting(pfds, timeout)?;
    for (r, p) in ready.iter_mut().zip(pfds.iter()) {
        *r = Ready::from_revents(p.revents);
    }
    Ok(())
}

/// Waits until one of `fds` is ready or the timeout passes (`None`: forever).
/// Returns what each descriptor reported, in order (all empty on a timeout).
/// Errors and hang-ups are always reported.
pub fn poll_ready(
    fds: &[(BorrowedFd<'_>, Wait)],
    timeout: Option<Duration>,
) -> io::Result<Vec<Ready>> {
    let mut ready = vec![Ready::default(); fds.len()];
    poll_ready_into(fds, timeout, &mut ready)?;
    Ok(ready)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsFd;

    #[test]
    fn poll_ready_into_does_not_allocate_for_a_frames_descriptors() {
        let (a, mut aw) = std::io::pipe().unwrap();
        let (b, _bw) = std::io::pipe().unwrap();
        let (c, _cw) = std::io::pipe().unwrap();
        aw.write_all(b"x").unwrap();
        let fds = [
            (a.as_fd(), Wait::READABLE),
            (b.as_fd(), Wait::PRIORITY),
            (c.as_fd(), Wait::NONE),
        ];
        let t = Some(Duration::ZERO);
        let mut ready = [Ready::default(); 3];
        for _ in 0..3 {
            let (r, n) = crate::test_alloc::allocations(|| poll_ready_into(&fds, t, &mut ready));
            r.unwrap();
            assert_eq!(n, 0, "poll_ready_into allocates");
        }
        assert!(ready[0].readable && !ready[1].readable && !ready[2].readable);
        // The Vec form keeps its contract: the same answers, in order (and it does
        // allocate, which shows the counter sees this thread's allocations).
        let (v, n) = crate::test_alloc::allocations(|| poll_ready(&fds, t));
        assert_eq!(v.unwrap()[..], ready[..]);
        assert!(n > 0, "the counter sees no allocation");
    }

    #[test]
    fn polls_several_descriptors() {
        let (a, mut aw) = std::io::pipe().unwrap();
        let (b, bw) = std::io::pipe().unwrap();
        let t = Some(Duration::from_millis(1));
        let r = poll_ready(
            &[(a.as_fd(), Wait::READABLE), (b.as_fd(), Wait::READABLE)],
            t,
        )
        .unwrap();
        assert!(r.iter().all(Ready::is_empty));
        aw.write_all(b"x").unwrap();
        drop(bw);
        let r = poll_ready(&[(a.as_fd(), Wait::READABLE), (b.as_fd(), Wait::NONE)], t).unwrap();
        assert!(r[0].readable && !r[0].hangup);
        assert!(r[1].hangup, "{:?}", r[1]);
        assert_eq!(
            Wait::READABLE.union(Wait::PRIORITY).events(),
            POLLIN | POLLPRI
        );
    }

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

    /// CPU time the calling thread has used, from `/proc/thread-self/stat`.
    fn thread_cpu() -> Duration {
        let stat = std::fs::read_to_string("/proc/thread-self/stat").unwrap();
        // Fields after the command name, which is in parentheses: utime is field 14
        // and stime field 15 of stat(5), so indices 11 and 12 here.
        let after = stat.rsplit_once(") ").unwrap().1;
        let fields: Vec<&str> = after.split_whitespace().collect();
        let ticks: u64 = fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
        // SAFETY: `sysconf` takes no pointers and only reads the clock tick rate.
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
        Duration::from_millis(ticks * 1000 / hz)
    }

    /// An idle wait sleeps: a 300 ms poll on a silent socket returns with no
    /// events, and the thread uses almost none of that time. A reaper that spun
    /// here would burn a CPU while a device is idle.
    #[test]
    fn an_idle_wait_sleeps_instead_of_spinning() {
        use std::os::fd::AsFd;
        use std::os::unix::net::UnixStream;
        let (quiet, _peer) = UnixStream::pair().unwrap();
        let before = thread_cpu();
        let started = std::time::Instant::now();
        let revents = poll_one(quiet.as_fd(), POLLIN, Some(Duration::from_millis(300))).unwrap();
        let waited = started.elapsed();
        let used = thread_cpu() - before;
        assert_eq!(revents, 0, "nothing to read: the wait ends on its timeout");
        assert!(
            waited >= Duration::from_millis(250),
            "the wait lasted: {waited:?}"
        );
        assert!(
            used < Duration::from_millis(50),
            "the wait used {used:?} of CPU"
        );
    }

    /// A hung-up peer reports at once (`POLLHUP`), so a loop that polls it
    /// without reaping spins: the caller must fail on the flag, as the USB reaper does.
    #[test]
    fn a_hung_up_descriptor_reports_at_once() {
        use std::os::fd::AsFd;
        use std::os::unix::net::UnixStream;
        let (quiet, peer) = UnixStream::pair().unwrap();
        drop(peer);
        let revents = poll_one(quiet.as_fd(), POLLIN, Some(Duration::from_secs(5))).unwrap();
        assert_ne!(revents & POLLHUP, 0, "a hang-up is reported");
    }
}
