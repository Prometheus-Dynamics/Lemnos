//! A Unix stream connect that never waits: a listener whose backlog is full answers `EAGAIN`
//! at once, and one that is not yet accepting answers `EINPROGRESS`, where the std connect would
//! block the calling thread until the listener takes the connection.

use std::io;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// What a non-blocking connect came to.
#[derive(Debug)]
pub enum Connect {
    /// The listener took the connection. The socket is closed again at once.
    Connected,
    /// The listener is not taking connections now (its backlog is full): try again later.
    Busy,
}

/// Connects to the Unix stream socket at `path` without waiting. Errors other than `Busy` (no
/// such socket, nothing listening, a path too long for the kernel) are returned as they are.
pub fn connect_nonblocking(path: &Path) -> io::Result<Connect> {
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: all-zero bytes are a valid `sockaddr_un` (integers and a byte array only).
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.is_empty() || bytes.len() >= addr.sun_path.len() {
        return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
    }
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (slot, byte) in addr.sun_path.iter_mut().zip(bytes) {
        *slot = *byte as libc::c_char;
    }
    // SAFETY: plain socket creation with constant arguments; the result is checked below.
    let raw = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `socket` returned a fresh descriptor nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let len =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    // SAFETY: `addr` is a live `sockaddr_un` and `len` covers its family, the path and its NUL;
    // `fd` is the socket created above.
    let r = unsafe {
        libc::connect(
            std::os::fd::AsRawFd::as_raw_fd(&fd),
            (&raw const addr).cast(),
            len,
        )
    };
    if r == 0 {
        return Ok(Connect::Connected);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::EAGAIN) | Some(libc::EINPROGRESS) => Ok(Connect::Busy),
        _ => Err(error),
    }
}
