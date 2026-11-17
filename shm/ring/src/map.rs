//! The only `unsafe` in the ring: making a sealed memfd and mapping it as atomic words.

use std::fs::File;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Mem, MemMut, RingError};

/// The seals a ring's memory carries: its size can never change. `F_SEAL_SEAL` is left off on
/// purpose: when the owner hands the memory to readers it adds `F_SEAL_FUTURE_WRITE` with
/// [`share_read_only`], and a seal can only be added before `F_SEAL_SEAL`. Nothing else can add
/// one: a reader holds only read-only descriptors, and those cannot.
const SEALS: libc::c_int = libc::F_SEAL_SHRINK | libc::F_SEAL_GROW;

/// A shared mapping of a descriptor, as 64-bit words.
#[derive(Debug)]
struct Mapping {
    base: NonNull<AtomicU64>,
    words: usize,
    bytes: usize,
    fd: OwnedFd,
}

// SAFETY: the mapping is plain shared memory reached only through atomic loads and stores, which
// are sound from any thread; the descriptor is an `OwnedFd`, itself `Send + Sync`.
unsafe impl Send for Mapping {}
// SAFETY: as above.
unsafe impl Sync for Mapping {}

impl Mapping {
    fn new(fd: OwnedFd, writable: bool) -> Result<Self, RingError> {
        // SAFETY: plain old data; all zeroes is a valid `stat`.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is open and `stat` is a valid place to write one `stat`.
        if unsafe { libc::fstat(fd.as_raw_fd(), &raw mut stat) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        // A file that can shrink would turn a later read into a fault. Only a memfd can be
        // sealed; anything else fails `F_GET_SEALS` and is refused the same way.
        // SAFETY: `fd` is open; `F_GET_SEALS` takes no argument.
        let seals = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GET_SEALS) };
        if seals < 0 || seals & libc::F_SEAL_SHRINK == 0 {
            return Err(RingError::Unsealed);
        }
        let bytes =
            usize::try_from(stat.st_size).map_err(|_| RingError::Malformed("size out of range"))?;
        if bytes < 8 {
            return Err(RingError::Malformed("shorter than a header"));
        }
        let protection = if writable {
            libc::PROT_READ | libc::PROT_WRITE
        } else {
            libc::PROT_READ
        };
        // SAFETY: a fresh shared mapping of `bytes` bytes of an open descriptor, placed by the
        // kernel. It aliases no Rust object.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes,
                protection,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(io::Error::last_os_error().into());
        }
        let base = NonNull::new(base.cast::<AtomicU64>())
            .ok_or(RingError::Malformed("mapped at address zero"))?;
        Ok(Self {
            base,
            words: bytes / 8,
            bytes,
            fd,
        })
    }

    fn word(&self, index: usize) -> Option<&AtomicU64> {
        if index >= self.words {
            return None;
        }
        // SAFETY: `index < words`, so the result stays inside the mapping.
        let word = unsafe { self.base.add(index) };
        // SAFETY: the word lies inside the mapping, which lives as long as `self` and cannot
        // shrink (the descriptor is sealed). A mapping is page-aligned, so every word is aligned
        // for `AtomicU64`. Other processes change this memory, which is exactly what an atomic
        // permits; a read-only mapping is only ever loaded from.
        Some(unsafe { word.as_ref() })
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: this is the mapping made in `new`, unmapped once, and no reference into it
        // outlives `self`.
        unsafe { libc::munmap(self.base.as_ptr().cast(), self.bytes) };
    }
}

/// A ring's memory mapped read-only. What a subscriber holds.
#[derive(Debug)]
pub struct ReadMap(Mapping);

impl ReadMap {
    /// Maps `fd` for reading.
    ///
    /// # Errors
    ///
    /// [`RingError::Unsealed`] if `fd` could shrink; [`RingError::Io`] if it can't be mapped.
    pub fn new(fd: OwnedFd) -> Result<Self, RingError> {
        Mapping::new(fd, false).map(Self)
    }
}

impl Mem for ReadMap {
    fn words(&self) -> usize {
        self.0.words
    }

    fn load(&self, index: usize, order: Ordering) -> u64 {
        self.0.word(index).map_or(0, |word| word.load(order))
    }
}

/// A ring's memory mapped for writing. What the publisher and the ring's owner hold.
#[derive(Debug)]
pub struct WriteMap(Mapping);

impl WriteMap {
    /// Maps `fd` for reading and writing.
    ///
    /// # Errors
    ///
    /// [`RingError::Unsealed`] if `fd` could shrink; [`RingError::Io`] if it can't be mapped
    /// writable, which is what a read end answers.
    pub fn new(fd: OwnedFd) -> Result<Self, RingError> {
        Mapping::new(fd, true).map(Self)
    }

    /// The descriptor behind the mapping.
    #[must_use]
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.0.fd.as_fd()
    }
}

impl Mem for WriteMap {
    fn words(&self) -> usize {
        self.0.words
    }

    fn load(&self, index: usize, order: Ordering) -> u64 {
        self.0.word(index).map_or(0, |word| word.load(order))
    }
}

impl MemMut for WriteMap {
    fn store(&self, index: usize, value: u64, order: Ordering) {
        if let Some(word) = self.0.word(index) {
            word.store(value, order);
        }
    }
}

/// A zeroed memfd of at least `bytes` bytes, rounded up to pages and sealed against resizing.
/// What a ring lives in, and the other shared pages built on the same words.
///
/// # Errors
///
/// [`RingError::Io`] if the memory can't be made.
pub fn sealed_memfd(bytes: usize) -> Result<OwnedFd, RingError> {
    // SAFETY: the name is a NUL-terminated literal and the flags are valid.
    let raw = unsafe {
        libc::memfd_create(
            c"shm-ring".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: `raw` is a descriptor this call just made and nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: `sysconf` reads a constant.
    let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap_or(4096);
    let size = bytes
        .max(1)
        .checked_next_multiple_of(page.max(8))
        .and_then(|size| libc::off_t::try_from(size).ok())
        .ok_or(RingError::Spec("ring is too large"))?;
    // SAFETY: `fd` is open.
    if unsafe { libc::ftruncate(fd.as_raw_fd(), size) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: `fd` is open and `SEALS` is a valid seal set.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_ADD_SEALS, SEALS) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(fd)
}

/// Makes a sealed memfd something outside readers can hold and nothing can write through, once its
/// owner has its own writable mapping. Adds `F_SEAL_FUTURE_WRITE` (no `write(2)` and no new
/// writable mapping, whichever descriptor asks) and sets the mode to `0444` (a read-write reopen
/// through `/proc` is refused to an unprivileged process). Existing writable mappings, the
/// owner's among them, keep working. Idempotent. Refused for a descriptor that can't be sealed,
/// such as a dma-buf, before anything changes.
///
/// # Errors
///
/// [`RingError::Io`] if the seal or the mode can't be set.
pub fn share_read_only(fd: BorrowedFd<'_>) -> Result<(), RingError> {
    // SAFETY: `fd` is open, and F_ADD_SEALS takes one integer seal set.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_ADD_SEALS, libc::F_SEAL_FUTURE_WRITE) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: `fd` is open, and fchmod takes only the mode.
    if unsafe { libc::fchmod(fd.as_raw_fd(), 0o444) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

/// A pipe for a reader's liveness: the read end is the host's, the write end goes to the reader,
/// and nothing is ever written to it. The read end reports `POLLHUP` once every write end is
/// closed, which is how the host sees that the reader has gone.
///
/// # Errors
///
/// [`RingError::Io`] if the pipe can't be made.
pub fn liveness_pipe() -> Result<(OwnedFd, OwnedFd), RingError> {
    let mut fds = [-1_i32; 2];
    // SAFETY: `fds` has room for the two descriptors `pipe2` writes.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: `fds[0]` was just made by `pipe2` and nothing else owns it.
    let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    // SAFETY: `fds[1]` was just made by `pipe2` and nothing else owns it.
    let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    Ok((read, write))
}

/// Whether `fd` was opened read-only, as its file mode says: a dma-buf allocated read-only stays
/// so, and the only way to a writable mapping of it is a writable descriptor.
///
/// # Errors
///
/// [`RingError::Io`] if the descriptor's flags can't be read.
pub fn is_read_only(fd: BorrowedFd<'_>) -> Result<bool, RingError> {
    // SAFETY: `fd` is open, and F_GETFL takes no argument.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(flags & libc::O_ACCMODE == libc::O_RDONLY)
}

/// Whether every write end of the pipe `fd` is closed. Never waits.
///
/// # Errors
///
/// [`RingError::Io`] if the poll fails.
pub fn hung_up(fd: BorrowedFd<'_>) -> Result<bool, RingError> {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one pollfd, and a zero timeout, so the call returns at once.
    let ready = unsafe { libc::poll(&raw mut poll, 1, 0) };
    if ready < 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(poll.revents & libc::POLLHUP != 0)
}

/// A second descriptor for the same memory that can only be read and only be mapped read-only.
///
/// # Errors
///
/// If the descriptor can't be reopened, which needs `/proc`.
pub fn read_only(fd: BorrowedFd<'_>) -> io::Result<OwnedFd> {
    File::open(format!("/proc/self/fd/{}", fd.as_raw_fd())).map(OwnedFd::from)
}
