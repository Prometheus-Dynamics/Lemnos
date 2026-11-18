//! A TCP connect that never waits. A non-blocking TCP connect answers `EINPROGRESS` even when
//! the peer is this machine, so the socket is asked once, without waiting, whether the handshake
//! is already done: to an address of this machine it is, both ways (taken or refused). A connect
//! that is still on its way is handed back, for the caller to ask again later with [`finished`].

use std::io;
use std::net::SocketAddr;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::time::Duration;

use crate::poll::{POLLOUT, poll_one};

/// What a non-blocking connect came to.
#[derive(Debug)]
pub enum Connect {
    /// The peer took the connection. The socket is closed again at once: nothing was sent.
    Connected,
    /// The handshake has not finished. Ask [`finished`] later; dropping the socket gives it up.
    Pending(OwnedFd),
}

/// Connects to `addr` without waiting. A scoped IPv6 address carries its interface in
/// [`std::net::SocketAddrV6::scope_id`] (see [`interface_index`]). An error is the kernel's
/// answer as it is: refused, no route, no such address, a family the process may not use.
pub fn connect_nonblocking(addr: &SocketAddr) -> io::Result<Connect> {
    // SAFETY: all-zero bytes are a valid `sockaddr_storage` (integers and byte arrays only).
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let (family, len) = match addr {
        SocketAddr::V4(v4) => {
            let sin = libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: v4.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(v4.ip().octets()),
                },
                sin_zero: [0; 8],
            };
            // SAFETY: `storage` is larger than and at least as aligned as `sockaddr_in`, and
            // is exclusively borrowed here.
            unsafe { (&raw mut storage).cast::<libc::sockaddr_in>().write(sin) };
            (libc::AF_INET, std::mem::size_of::<libc::sockaddr_in>())
        }
        SocketAddr::V6(v6) => {
            let sin6 = libc::sockaddr_in6 {
                sin6_family: libc::AF_INET6 as libc::sa_family_t,
                sin6_port: v6.port().to_be(),
                sin6_flowinfo: 0,
                sin6_addr: libc::in6_addr {
                    s6_addr: v6.ip().octets(),
                },
                sin6_scope_id: v6.scope_id(),
            };
            // SAFETY: `storage` is larger than and at least as aligned as `sockaddr_in6`, and
            // is exclusively borrowed here.
            unsafe { (&raw mut storage).cast::<libc::sockaddr_in6>().write(sin6) };
            (libc::AF_INET6, std::mem::size_of::<libc::sockaddr_in6>())
        }
    };
    // SAFETY: plain socket creation with constant arguments; the result is checked below.
    let raw = unsafe {
        libc::socket(
            family,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `socket` returned a fresh descriptor nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: `storage` holds a socket address of `family` in its first `len` bytes, alive for
    // the call; `fd` is the socket created above.
    let r = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            (&raw const storage).cast(),
            len as libc::socklen_t,
        )
    };
    if r == 0 {
        return Ok(Connect::Connected);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(libc::EINPROGRESS) {
        return Err(error);
    }
    if finished(std::os::fd::AsFd::as_fd(&fd))? {
        Ok(Connect::Connected)
    } else {
        Ok(Connect::Pending(fd))
    }
}

/// Whether the connect [`connect_nonblocking`] left pending on `fd` has finished, asked without
/// waiting: `true` once the peer took the connection, `false` while the handshake is still on
/// its way, and the error the connect ended with (refused, no route, timed out) when it failed.
pub fn finished(fd: BorrowedFd<'_>) -> io::Result<bool> {
    let ready = poll_one(fd, POLLOUT, Some(Duration::ZERO))?;
    if ready == 0 {
        return Ok(false);
    }
    let mut code: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: `code` is writable for `len` bytes and `len` is writable, both alive for the
    // call; `fd` is open for the call.
    let rc = unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            (&raw mut code).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    if code != 0 {
        return Err(io::Error::from_raw_os_error(code));
    }
    if ready & POLLOUT != 0 {
        Ok(true)
    } else {
        // Hung up with no error kept: the connect is over and nothing is connected.
        Err(io::Error::from_raw_os_error(libc::ECONNABORTED))
    }
}

/// The index of the network interface named `name` now (`if_nametoindex`), for an IPv6 scope.
/// `ENODEV` when no interface has that name, or when it is not a name an interface can have
/// (empty, 16 bytes or longer, or holding a NUL). Nothing is allocated.
pub fn interface_index(name: &str) -> io::Result<u32> {
    let bytes = name.as_bytes();
    let mut text = [0 as libc::c_char; libc::IF_NAMESIZE];
    if bytes.is_empty() || bytes.len() >= text.len() || bytes.contains(&0) {
        return Err(io::Error::from_raw_os_error(libc::ENODEV));
    }
    for (slot, byte) in text.iter_mut().zip(bytes) {
        *slot = *byte as libc::c_char;
    }
    // SAFETY: `text` is a NUL-terminated string (its last byte is never written), alive for
    // the call.
    let index = unsafe { libc::if_nametoindex(text.as_ptr()) };
    if index == 0 {
        return Err(io::Error::from_raw_os_error(libc::ENODEV));
    }
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listener_on_this_machine_is_connected_at_once_and_a_closed_port_refused() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        assert!(matches!(
            connect_nonblocking(&addr).unwrap(),
            Connect::Connected
        ));
        // The same port on another loopback address: held by `listener` against a bind to every
        // address, and nothing listens on it there.
        let closed = SocketAddr::from(([127, 0, 0, 5], addr.port()));
        let error = connect_nonblocking(&closed).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ECONNREFUSED));
    }

    #[test]
    fn an_interface_is_found_by_name_and_a_missing_one_is_no_such_device() {
        assert!(interface_index("lo").unwrap() > 0);
        for name in [
            "lemnosnone0",
            "",
            "a-name-longer-than-an-interface-has",
            "a\0b",
        ] {
            let error = interface_index(name).unwrap_err();
            assert_eq!(error.raw_os_error(), Some(libc::ENODEV), "{name:?}");
        }
    }
}
