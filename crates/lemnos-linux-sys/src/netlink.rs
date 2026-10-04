//! The kernel's uevent socket (`NETLINK_KOBJECT_UEVENT`): devices appearing,
//! going away, binding to drivers. What udev listens to, without udev.
//! Unprivileged processes may listen. Ported from Styx
//! (`styx-kernel/src/uevent.rs`).

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

const NETLINK_KOBJECT_UEVENT: libc::c_int = 15;
/// The kernel's multicast group (udev re-broadcasts on group 2).
pub const KERNEL_GROUP: u32 = 1;

/// Opens a non-blocking, close-on-exec uevent socket bound to `groups`
/// ([`KERNEL_GROUP`] for the kernel's own events).
pub fn uevent_socket(groups: u32) -> io::Result<OwnedFd> {
    // SAFETY: plain socket creation with constant arguments; the result is
    // checked below.
    let raw = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            NETLINK_KOBJECT_UEVENT,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `socket` returned a fresh descriptor nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: all-zero bytes are a valid `sockaddr_nl` (integers only).
    let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
    addr.nl_groups = groups;
    // SAFETY: `addr` is a live, valid `sockaddr_nl` and the length passed is
    // its size; `fd` is the socket created above.
    let r = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            (&raw const addr).cast(),
            size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    };
    if r < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

/// Receives one datagram into `buf`. Returns the length and the sender's
/// netlink port id (0: the kernel), or `None` when nothing is pending.
/// `ENOBUFS` (the receive queue overflowed; events were lost) is returned as
/// an error of kind `Other` with that errno.
pub fn recv(fd: BorrowedFd<'_>, buf: &mut [u8]) -> io::Result<Option<(usize, u32)>> {
    loop {
        // SAFETY: all-zero bytes are a valid `sockaddr_nl`.
        let mut from: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        let mut from_len = size_of::<libc::sockaddr_nl>() as libc::socklen_t;
        // SAFETY: `buf` and `from` are live, exclusively borrowed and writable
        // for the lengths given; `fd` is a live descriptor.
        let n = unsafe {
            libc::recvfrom(
                fd.as_raw_fd(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                0,
                (&raw mut from).cast(),
                &mut from_len,
            )
        };
        if n >= 0 {
            return Ok(Some((n as usize, from.nl_pid)));
        }
        let e = io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EAGAIN) => return Ok(None),
            _ => return Err(e),
        }
    }
}

/// Whether an error from [`recv`] means the queue overflowed (`ENOBUFS`).
pub fn is_overflow(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::ENOBUFS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    #[test]
    fn opens_where_netlink_is_allowed() {
        let Ok(sock) = uevent_socket(KERNEL_GROUP) else {
            return;
        };
        let mut buf = [0u8; 64];
        assert!(recv(sock.as_fd(), &mut buf).is_ok());
    }
}
