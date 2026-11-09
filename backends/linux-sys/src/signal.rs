//! Receiving signals as a readable descriptor (`signalfd`), so an event loop
//! handles `SIGTERM` like any other input.

use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

/// `SIGTERM`: a request to stop (systemd's stop signal).
pub const SIGTERM: i32 = libc::SIGTERM;
/// `SIGINT`: Ctrl-C.
pub const SIGINT: i32 = libc::SIGINT;
/// `SIGHUP`: often a request to reload.
pub const SIGHUP: i32 = libc::SIGHUP;

/// Pending signals as a descriptor.
#[derive(Debug)]
pub struct SignalFd {
    fd: OwnedFd,
}

impl SignalFd {
    /// Blocks `signals` for the calling thread (threads it starts afterwards
    /// inherit the mask) and returns a non-blocking descriptor that becomes
    /// readable while one of them is pending. Create it before starting
    /// other threads, or block the signals there too.
    pub fn new(signals: &[i32]) -> io::Result<Self> {
        let mut set = MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: `sigemptyset` initializes the set it is given.
        if unsafe { libc::sigemptyset(set.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `sigemptyset` succeeded, so `set` is initialized.
        let mut set = unsafe { set.assume_init() };
        for signal in signals {
            // SAFETY: `set` is an initialized `sigset_t`.
            if unsafe { libc::sigaddset(&raw mut set, *signal) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        // SAFETY: `set` is initialized; a null old-set pointer is allowed.
        let r =
            unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &raw const set, std::ptr::null_mut()) };
        if r != 0 {
            return Err(io::Error::from_raw_os_error(r));
        }
        // SAFETY: -1 asks for a new descriptor; `set` is initialized.
        let fd =
            unsafe { libc::signalfd(-1, &raw const set, libc::SFD_CLOEXEC | libc::SFD_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `signalfd` returned a new descriptor that nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok(Self { fd })
    }

    /// The next pending signal's number, `None` when none is pending.
    pub fn read(&self) -> io::Result<Option<i32>> {
        let mut info = MaybeUninit::<libc::signalfd_siginfo>::uninit();
        let size = std::mem::size_of::<libc::signalfd_siginfo>();
        // SAFETY: `info` is writable and `size` bytes long; the kernel writes
        // whole `signalfd_siginfo` records only.
        let n = unsafe { libc::read(self.fd.as_raw_fd(), info.as_mut_ptr().cast(), size) };
        if n < 0 {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::WouldBlock {
                Ok(None)
            } else {
                Err(e)
            };
        }
        if n as usize != size {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        // SAFETY: the kernel filled the whole record.
        let info = unsafe { info.assume_init() };
        Ok(Some(info.ssi_signo as i32))
    }
}

impl AsFd for SignalFd {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivers_a_blocked_signal_as_a_readable_descriptor() {
        // A signal nothing else in the test process uses.
        let signals = SignalFd::new(&[libc::SIGUSR2]).unwrap();
        assert_eq!(signals.read().unwrap(), None);
        // SAFETY: `pthread_self` has no preconditions.
        let me = unsafe { libc::pthread_self() };
        // SAFETY: `me` is this live thread; the signal is blocked on it, so it
        // is only queued.
        unsafe { libc::pthread_kill(me, libc::SIGUSR2) };
        assert_eq!(signals.read().unwrap(), Some(libc::SIGUSR2));
    }
}
