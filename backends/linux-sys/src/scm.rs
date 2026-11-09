//! Passing descriptors over a Unix socket (`SCM_RIGHTS`).
//!
//! The control message is built and read as plain bytes, so only the system calls and the taking
//! of a descriptor are `unsafe`. A received descriptor is owned from the moment it is found in the
//! control message, so it closes on every path.

use std::io;
use std::mem::{align_of, size_of};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

/// The most descriptors carried in one message.
pub const MAX_FDS: usize = 16;

const FD_BYTES: usize = size_of::<RawFd>();
const CONTROL_ALIGN: usize = align_of::<usize>();
const HEADER: usize = align_up(size_of::<libc::cmsghdr>());
/// Room for a control message of [`MAX_FDS`] descriptors.
const CONTROL_BYTES: usize = align_up(HEADER + MAX_FDS * FD_BYTES);

const fn align_up(bytes: usize) -> usize {
    (bytes + CONTROL_ALIGN - 1) & !(CONTROL_ALIGN - 1)
}

/// Aligned for the `cmsghdr` the kernel reads and writes in it.
#[repr(align(8))]
struct Control([u8; CONTROL_BYTES]);

/// One `msghdr` over one buffer and the control bytes.
fn message(iov: *mut libc::iovec, control: *mut u8, control_len: usize) -> libc::msghdr {
    // SAFETY: `msghdr` is plain data, and all zeroes is a valid value of it.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.cast();
    msg.msg_controllen = control_len as _;
    msg
}

fn control_header(fds: usize) -> libc::cmsghdr {
    // SAFETY: `cmsghdr` is plain data, and all zeroes is a valid value of it.
    let mut header: libc::cmsghdr = unsafe { std::mem::zeroed() };
    header.cmsg_len = (HEADER + fds * FD_BYTES) as _;
    header.cmsg_level = libc::SOL_SOCKET;
    header.cmsg_type = libc::SCM_RIGHTS;
    header
}

/// Sends `bytes` with `fds` attached to the first byte. Returns how much of `bytes` went, which
/// can be less than all of them; the descriptors went with the first byte.
pub fn send_with_fds(
    socket: BorrowedFd<'_>,
    bytes: &[u8],
    fds: &[BorrowedFd<'_>],
) -> io::Result<usize> {
    if bytes.is_empty() || fds.is_empty() || fds.len() > MAX_FDS {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let mut control = Control([0; CONTROL_BYTES]);
    let raw_header = control_header(fds.len());
    // SAFETY: the array is initialised and is exactly `size_of::<cmsghdr>()` bytes of the
    // `cmsghdr` just built; only its bytes are copied.
    let header_bytes = unsafe {
        std::slice::from_raw_parts(
            (&raw const raw_header).cast::<u8>(),
            size_of::<libc::cmsghdr>(),
        )
    };
    control.0[..size_of::<libc::cmsghdr>()].copy_from_slice(header_bytes);
    let slots = control.0[HEADER..].as_chunks_mut::<FD_BYTES>().0;
    for (fd, slot) in fds.iter().zip(slots) {
        *slot = fd.as_raw_fd().to_ne_bytes();
    }
    let control_len = align_up(HEADER + fds.len() * FD_BYTES);
    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    let msg = message(&raw mut iov, control.0.as_mut_ptr(), control_len);
    // SAFETY: `msg` names `iov`, which names `bytes`, and the control bytes above, all live for
    // the call. `sendmsg` reads them and keeps none of it.
    let sent = unsafe { libc::sendmsg(socket.as_raw_fd(), &raw const msg, libc::MSG_NOSIGNAL) };
    if sent < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(sent as usize)
}

/// Receives into `buf`, appending any descriptors that came to `fds`. Returns the bytes read, 0
/// at end of file. A control message that didn't fit is an error, and whatever did arrive is
/// already in `fds`, to be closed by the caller's dropping them.
pub fn recv_with_fds(
    socket: BorrowedFd<'_>,
    buf: &mut [u8],
    fds: &mut Vec<OwnedFd>,
) -> io::Result<usize> {
    let mut control = Control([0; CONTROL_BYTES]);
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: buf.len(),
    };
    let mut msg = message(&raw mut iov, control.0.as_mut_ptr(), CONTROL_BYTES);
    // SAFETY: `msg` names `iov`, which names `buf`, and the control bytes, all live and writable
    // for the call. The kernel writes no more than the lengths they carry.
    let read = unsafe { libc::recvmsg(socket.as_raw_fd(), &raw mut msg, libc::MSG_CMSG_CLOEXEC) };
    if read < 0 {
        return Err(io::Error::last_os_error());
    }
    // `msg_controllen` is `usize` on glibc and `u32` on musl.
    #[allow(clippy::unnecessary_cast)]
    let filled = (msg.msg_controllen as usize).min(CONTROL_BYTES);
    take_descriptors(&control.0, filled, fds);
    if msg.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    Ok(read as usize)
}

/// Walks the control messages the kernel wrote, taking ownership of every descriptor.
fn take_descriptors(control: &[u8], control_len: usize, fds: &mut Vec<OwnedFd>) {
    let usable = control.get(..control_len).unwrap_or(&[]);
    let mut at = 0;
    while let Some(rest) = usable.get(at..) {
        if rest.len() < HEADER {
            return;
        }
        let Some(len_bytes) = rest.first_chunk::<{ size_of::<usize>() }>() else {
            return;
        };
        let len = usize::from_ne_bytes(*len_bytes);
        if len < HEADER || len > rest.len() {
            return;
        }
        let level_at = size_of::<usize>();
        let level = i32_at(rest, level_at);
        let kind = i32_at(rest, level_at + 4);
        if level == Some(libc::SOL_SOCKET) && kind == Some(libc::SCM_RIGHTS) {
            for chunk in rest
                .get(HEADER..len)
                .unwrap_or(&[])
                .as_chunks::<FD_BYTES>()
                .0
            {
                let raw = RawFd::from_ne_bytes(*chunk);
                // SAFETY: `SCM_RIGHTS` gave this process a new descriptor; nothing else owns it.
                fds.push(unsafe { OwnedFd::from_raw_fd(raw) });
            }
        }
        at += align_up(len).max(HEADER);
    }
}

fn i32_at(bytes: &[u8], at: usize) -> Option<i32> {
    let chunk = bytes.get(at..at + 4)?;
    <[u8; 4]>::try_from(chunk).ok().map(i32::from_ne_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dma_buf::memfd;
    use std::io::{Read, Write};
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixStream;

    #[test]
    fn descriptors_arrive_owned_with_the_bytes_and_still_work() {
        let (a, b) = UnixStream::pair().unwrap();
        let memory = memfd(c"scm").unwrap();
        let mut file = std::fs::File::from(memory.try_clone().unwrap());
        file.write_all(b"ok").unwrap();
        let (x, y) = (a.as_fd().try_clone_to_owned().unwrap(), memory);
        assert_eq!(
            send_with_fds(a.as_fd(), b"hello", &[x.as_fd(), y.as_fd()]).unwrap(),
            5
        );
        drop(a);
        let mut buf = [0u8; 16];
        let mut fds = Vec::new();
        assert_eq!(recv_with_fds(b.as_fd(), &mut buf, &mut fds).unwrap(), 5);
        assert_eq!(&buf[..5], b"hello");
        assert_eq!(fds.len(), 2);
        let mut read_back = std::fs::File::from(fds.remove(1));
        use std::io::Seek;
        read_back.seek(std::io::SeekFrom::Start(0)).unwrap();
        let mut text = String::new();
        read_back.read_to_string(&mut text).unwrap();
        assert_eq!(text, "ok");
    }

    #[test]
    fn plain_bytes_arrive_with_no_descriptors() {
        let (mut a, b) = UnixStream::pair().unwrap();
        a.write_all(b"xy").unwrap();
        let mut buf = [0u8; 4];
        let mut fds = Vec::new();
        assert_eq!(recv_with_fds(b.as_fd(), &mut buf, &mut fds).unwrap(), 2);
        assert!(fds.is_empty());
    }

    #[test]
    fn too_many_or_no_descriptors_are_refused() {
        let (a, _b) = UnixStream::pair().unwrap();
        let fd = a.as_fd();
        assert!(send_with_fds(fd, b"x", &[]).is_err());
        let many = vec![fd; MAX_FDS + 1];
        assert!(send_with_fds(fd, b"x", &many).is_err());
        assert!(send_with_fds(fd, b"", &[fd]).is_err());
    }

    #[test]
    fn descriptors_close_when_dropped() {
        let (a, b) = UnixStream::pair().unwrap();
        let (r, w) = (
            a.as_fd().try_clone_to_owned().unwrap(),
            a.as_fd().try_clone_to_owned().unwrap(),
        );
        send_with_fds(a.as_fd(), b"x", &[r.as_fd(), w.as_fd()]).unwrap();
        drop((r, w));
        let mut buf = [0u8; 4];
        let mut fds = Vec::new();
        recv_with_fds(b.as_fd(), &mut buf, &mut fds).unwrap();
        let numbers: Vec<RawFd> = fds.iter().map(|f| f.as_raw_fd()).collect();
        drop(fds);
        for number in numbers {
            // SAFETY: `fcntl(F_GETFD)` reads a descriptor number; `EBADF` is the expected answer.
            let result = unsafe { libc::fcntl(number, libc::F_GETFD) };
            assert_eq!(result, -1, "descriptor {number} still open");
        }
    }

    #[test]
    fn take_descriptors_ignores_control_messages_it_doesnt_know() {
        let mut control = [0u8; CONTROL_BYTES];
        let mut fds = Vec::new();
        // A length past the end, and a length of zero: neither reads anything or loops.
        control[..size_of::<usize>()].copy_from_slice(&1_000usize.to_ne_bytes());
        take_descriptors(&control, 32, &mut fds);
        control[..size_of::<usize>()].copy_from_slice(&0usize.to_ne_bytes());
        take_descriptors(&control, 32, &mut fds);
        assert!(fds.is_empty());
    }
}
