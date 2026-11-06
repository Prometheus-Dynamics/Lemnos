//! Device nodes: opening them, listing them, the typed ioctl call and the readiness waits.
//! Each system call is made through `lemnos-linux-sys`; errors leave here as [`Error`]s
//! naming the call.

use std::fs::OpenOptions;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lemnos_linux_sys::poll::{self as sys_poll, Ready, Wait};
use lemnos_linux_sys::v4l2::device::{Ioctl, Pod, set_nonblocking as sys_set_nonblocking};

use crate::{Error, Result};

/// Opens a device node close-on-exec, read-write or read-only, and optionally
/// non-blocking.
pub(crate) fn open(path: &Path, write: bool, nonblocking: bool) -> Result<OwnedFd> {
    let mut options = OpenOptions::new();
    options.read(true).write(write);
    if nonblocking {
        options.custom_flags(libc::O_NONBLOCK);
    }
    options
        .open(path)
        .map(OwnedFd::from)
        .map_err(|source| Error::Open {
            path: path.to_owned(),
            source,
        })
}

/// Issues a typed request on `fd` with `arg`, naming the request in the error.
pub(crate) fn ioctl<T: Pod>(fd: BorrowedFd<'_>, request: Ioctl<T>, arg: &mut T) -> Result<()> {
    request
        .call(fd, arg)
        .map(|_| ())
        .map_err(|source| Error::ioctl_io(request.name(), source))
}

/// Sets or clears `O_NONBLOCK` on `fd`.
pub(crate) fn set_nonblocking(fd: BorrowedFd<'_>, nonblocking: bool) -> Result<()> {
    sys_set_nonblocking(fd, nonblocking).map_err(|source| Error::sys_io("fcntl", source))
}

/// Waits (`poll(2)`) until `fd` has any of the `events` ready (`POLLIN`, `POLLOUT`,
/// `POLLPRI`), or the timeout passes (an empty [`Ready`]). `None` waits forever. Errors and
/// hang-ups are always reported.
pub(crate) fn wait_events(
    fd: BorrowedFd<'_>,
    events: i16,
    timeout: Option<Duration>,
) -> Result<Ready> {
    let revents =
        sys_poll::poll_one(fd, events, timeout).map_err(|source| Error::sys_io("ppoll", source))?;
    Ok(Ready::from_revents(revents))
}

/// Waits until one of `fds` is ready or the timeout passes (`None` waits forever). Returns what
/// each descriptor reported, in order (all empty on a timeout). Errors and hang-ups are always
/// reported.
pub fn poll(fds: &[(BorrowedFd<'_>, Wait)], timeout: Option<Duration>) -> Result<Vec<Ready>> {
    sys_poll::poll_ready(fds, timeout).map_err(|source| Error::sys_io("ppoll", source))
}

/// [`poll`] with the results written to `ready` (one per descriptor, in order), without
/// allocating for up to 8 descriptors.
///
/// # Panics
///
/// When `ready` is shorter than `fds`.
pub fn poll_into(
    fds: &[(BorrowedFd<'_>, Wait)],
    timeout: Option<Duration>,
    ready: &mut [Ready],
) -> Result<()> {
    sys_poll::poll_ready_into(fds, timeout, ready).map_err(|source| Error::sys_io("ppoll", source))
}

/// Lists `/dev/<prefix>N` nodes (e.g. `video0`, `media3`), sorted by `N`.
pub(crate) fn list_dev_nodes(prefix: &str) -> Vec<PathBuf> {
    let mut found: Vec<(u32, PathBuf)> = std::fs::read_dir("/dev")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let n = name.to_str()?.strip_prefix(prefix)?.parse::<u32>().ok()?;
            Some((n, entry.path()))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, p)| p).collect()
}

/// Converts a fixed-size, NUL-padded C string field to a `String` (lossy).
pub(crate) fn cstr_field(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsFd;

    #[test]
    fn cstr_fields_stop_at_nul() {
        assert_eq!(cstr_field(b"uvcvideo\0\0\0junk"), "uvcvideo");
        assert_eq!(cstr_field(b"full"), "full");
    }

    #[test]
    fn polls_several_descriptors() {
        let (a, mut aw) = std::io::pipe().unwrap();
        let (b, bw) = std::io::pipe().unwrap();
        let t = Some(Duration::from_millis(1));
        let r = poll(
            &[(a.as_fd(), Wait::READABLE), (b.as_fd(), Wait::READABLE)],
            t,
        )
        .unwrap();
        assert!(r.iter().all(Ready::is_empty));
        aw.write_all(b"x").unwrap();
        drop(bw);
        let r = poll(&[(a.as_fd(), Wait::READABLE), (b.as_fd(), Wait::NONE)], t).unwrap();
        assert!(r[0].readable && !r[0].hangup);
        assert!(r[1].hangup, "{:?}", r[1]);
    }

    #[test]
    fn a_single_wait_reports_what_is_ready() {
        let (a, mut aw) = std::io::pipe().unwrap();
        let zero = Some(Duration::ZERO);
        assert!(
            wait_events(a.as_fd(), sys_poll::POLLIN, zero)
                .unwrap()
                .is_empty()
        );
        aw.write_all(b"x").unwrap();
        let ready = wait_events(a.as_fd(), sys_poll::POLLIN, zero).unwrap();
        assert!(ready.readable && !ready.priority);
    }

    #[test]
    fn a_missing_node_is_an_open_error_with_its_path() {
        let err = open(Path::new("/nonexistent/lemnos-node"), true, true).unwrap_err();
        assert!(matches!(err, Error::Open { .. }), "{err:?}");
    }
}
