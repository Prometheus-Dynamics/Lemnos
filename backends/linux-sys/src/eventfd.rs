//! A counter one side adds to and another waits on: the doorbell a stream rings per frame.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

/// A new non-blocking, close-on-exec eventfd counting from zero.
pub fn create() -> io::Result<OwnedFd> {
    make(libc::EFD_NONBLOCK)
}

/// A new blocking, close-on-exec eventfd counting from zero: a read waits until it counts. The
/// flag is on the open file, so every duplicate of it blocks too.
pub fn create_blocking() -> io::Result<OwnedFd> {
    make(0)
}

fn make(flags: libc::c_int) -> io::Result<OwnedFd> {
    // SAFETY: `eventfd` takes only integers.
    let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `eventfd` returned a new descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Adds one. A full counter already says "something happened", so that isn't an error.
pub fn signal(fd: BorrowedFd<'_>) {
    let one = 1u64.to_ne_bytes();
    // SAFETY: `one` is 8 readable bytes, what an eventfd write takes, for the call.
    let _ = unsafe { libc::write(fd.as_raw_fd(), one.as_ptr().cast(), one.len()) };
}

/// Clears the counter, returning what it held. 0 when it was empty.
pub fn drain(fd: BorrowedFd<'_>) -> u64 {
    let mut value = [0u8; 8];
    // SAFETY: `value` is 8 writable bytes, what an eventfd read takes, for the call.
    let read = unsafe { libc::read(fd.as_raw_fd(), value.as_mut_ptr().cast(), value.len()) };
    if read == value.len() as isize {
        u64::from_ne_bytes(value)
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    #[test]
    fn signals_add_up_and_draining_clears() {
        let fd = create().unwrap();
        assert_eq!(drain(fd.as_fd()), 0);
        signal(fd.as_fd());
        signal(fd.as_fd());
        assert_eq!(drain(fd.as_fd()), 2);
        assert_eq!(drain(fd.as_fd()), 0);
    }
}
