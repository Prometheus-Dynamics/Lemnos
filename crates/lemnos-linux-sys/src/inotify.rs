//! inotify: watching directories for entries appearing and going away (the
//! hotplug fallback when uevent sockets are unavailable, and for test roots).
//! Events are read from the descriptor with `read(2)` (e.g. through
//! `std::fs::File`) and decoded with [`parse_events`].

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// `IN_*` event mask bits.
pub mod mask {
    /// Metadata changed.
    pub const ATTRIB: u32 = 0x0000_0004;
    /// Moved out of the watched directory.
    pub const MOVED_FROM: u32 = 0x0000_0040;
    /// Moved into the watched directory.
    pub const MOVED_TO: u32 = 0x0000_0080;
    /// Created in the watched directory.
    pub const CREATE: u32 = 0x0000_0100;
    /// Deleted from the watched directory.
    pub const DELETE: u32 = 0x0000_0200;
    /// The watched directory itself was deleted.
    pub const DELETE_SELF: u32 = 0x0000_0400;
    /// The watched directory itself was moved.
    pub const MOVE_SELF: u32 = 0x0000_0800;
    /// The event queue overflowed.
    pub const Q_OVERFLOW: u32 = 0x0000_4000;
    /// The watch was removed.
    pub const IGNORED: u32 = 0x0000_8000;
}

/// Opens a non-blocking, close-on-exec inotify instance.
pub fn init() -> io::Result<OwnedFd> {
    // SAFETY: plain syscall with constant flags; the result is checked.
    let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `inotify_init1` returned a fresh descriptor nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// Adds (or updates) a watch on `path`; returns the watch descriptor.
pub fn add_watch(fd: BorrowedFd<'_>, path: &Path, mask: u32) -> io::Result<i32> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    // SAFETY: `path` is a valid NUL-terminated string alive for the call; `fd`
    // is a live descriptor.
    let wd = unsafe { libc::inotify_add_watch(fd.as_raw_fd(), path.as_ptr(), mask) };
    if wd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(wd)
}

/// Removes a watch.
pub fn rm_watch(fd: BorrowedFd<'_>, wd: i32) -> io::Result<()> {
    // SAFETY: plain syscall on integers; `fd` is a live descriptor.
    let r = unsafe { libc::inotify_rm_watch(fd.as_raw_fd(), wd) };
    if r < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// One decoded inotify event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Watch descriptor (-1 for queue overflow).
    pub wd: i32,
    /// [`mask`] bits.
    pub mask: u32,
    /// Name of the entry in the watched directory, if any.
    pub name: Option<std::ffi::OsString>,
}

/// Decodes the events in a buffer filled by `read(2)` on an inotify
/// descriptor (`struct inotify_event` records, native endianness).
pub fn parse_events(mut bytes: &[u8]) -> Vec<Event> {
    const HEADER: usize = 16;
    let mut events = Vec::new();
    while bytes.len() >= HEADER {
        let word = |i: usize| [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]];
        let wd = i32::from_ne_bytes(word(0));
        let mask = u32::from_ne_bytes(word(4));
        let len = u32::from_ne_bytes(word(12)) as usize;
        let end = (HEADER + len).min(bytes.len());
        let raw_name = &bytes[HEADER..end];
        let trimmed = raw_name.split(|b| *b == 0).next().unwrap_or(&[]);
        let name =
            (!trimmed.is_empty()).then(|| std::ffi::OsStr::from_bytes(trimmed).to_os_string());
        events.push(Event { wd, mask, name });
        bytes = &bytes[end..];
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::fd::AsFd;

    #[test]
    fn watches_a_directory() {
        let dir = std::env::temp_dir().join(format!("lemnos-inotify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fd = init().unwrap();
        let wd = add_watch(fd.as_fd(), &dir, mask::CREATE | mask::DELETE).unwrap();
        std::fs::create_dir(dir.join("gpiochip7")).unwrap();
        let mut file = std::fs::File::from(fd);
        let mut buf = [0u8; 4096];
        let n = file.read(&mut buf).unwrap();
        let events = parse_events(&buf[..n]);
        assert_eq!(events[0].wd, wd);
        assert_eq!(events[0].mask & mask::CREATE, mask::CREATE);
        assert_eq!(
            events[0].name.as_deref(),
            Some(std::ffi::OsStr::new("gpiochip7"))
        );
        let err = file.read(&mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        rm_watch(file.as_fd(), wd).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
