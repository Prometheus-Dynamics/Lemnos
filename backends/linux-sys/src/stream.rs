//! A frame stream the Lemnos kernel module makes on a block (`LEMNOS_IOC_STREAM_CREATE`), for
//! tests and tools that drive the module's ring directly. Test support and the uAPI's own shapes:
//! a stream's ring is read through [`crate::mmap::Mapping`], and the module's layout is the
//! caller's to check.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

use crate::ioctl::{Ioctl, ioc, ioctl_ptr, iow, iowr, pod, zeroed};

const TYPE: u8 = b'L';

/// `LEMNOS_STREAM_CREATE_BUFFERS`: each buffer a dma-buf of its own, no pool.
pub const CREATE_BUFFERS: u32 = 1 << 0;
/// `LEMNOS_STREAM_CREATE_SHM`: the frame ring is the shared-memory ring of `shm/README.md`.
pub const CREATE_SHM: u32 = 1 << 1;

/// `struct lemnos_stream_create`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Create {
    block: u32,
    flags: u32,
    buffers: u32,
    buffer_bytes: u32,
    frame_slots: u32,
    job_slots: u32,
    job_bytes: u32,
    pool_fd: i32,
    doorbell_fd: i32,
    fd: i32,
    ring_bytes: u64,
    pool_addr: u64,
}
pod!(Create);

/// `struct lemnos_stream_test_source`.
#[repr(C)]
#[derive(Clone, Copy)]
struct TestSource {
    period_us: u32,
    frames: u32,
}
pod!(TestSource);

/// `struct lemnos_stream_reader`: the marks page and its limits in, the slot and the registration's
/// file out.
#[repr(C)]
#[derive(Clone, Copy)]
struct Reader {
    fd: i32,
    slot: u32,
    max_held: u32,
    reader_fd: i32,
    max_hold_ns: u64,
}
pod!(Reader);

const READER: Ioctl<Reader> = Ioctl::new(iowr::<Reader>(TYPE, 0x87), "LEMNOS_STREAM_IOC_READER");
/// `LEMNOS_READER_IOC_UNREGISTER`: `_IO`, no argument.
const UNREGISTER: u32 = ioc(0, TYPE, 0x88, 0);

const CREATE_REQ: Ioctl<Create> =
    Ioctl::new(iowr::<Create>(TYPE, 0x80), "LEMNOS_IOC_STREAM_CREATE");
const TEST_SOURCE: Ioctl<TestSource> = Ioctl::new(
    iow::<TestSource>(TYPE, 0x85),
    "LEMNOS_STREAM_IOC_TEST_SOURCE",
);

/// What a stream asks for. The pool, when there is one, is a dma-buf of `buffers` times the
/// buffers' aligned size, held by the caller until the stream is made.
#[derive(Debug, Clone, Copy)]
pub struct Spec<'a> {
    /// The block number, from `find_block`.
    pub block: u32,
    /// `CREATE_*` flags.
    pub flags: u32,
    /// Buffers.
    pub buffers: u32,
    /// Bytes per buffer.
    pub buffer_bytes: u32,
    /// Frame slots: a power of two, at least `buffers` (at least 2 for the shm form).
    pub frame_slots: u32,
    /// Job slots: a power of two.
    pub job_slots: u32,
    /// Bytes per job slot: the job structure's size.
    pub job_bytes: u32,
    /// The pool dma-buf, or none for a stream with buffers of their own.
    pub pool: Option<BorrowedFd<'a>>,
}

/// A stream the module made: its descriptor, and what to map for its ring.
#[derive(Debug)]
pub struct Stream {
    /// The stream's descriptor, close-on-exec.
    pub fd: OwnedFd,
    /// Bytes of the ring: what to map at offset 0.
    pub ring_bytes: usize,
    /// Buffer 0's device address, for a pool.
    pub pool_addr: u64,
}

/// Makes a stream on `node` (`/dev/lemnos`) as `spec` says, with no doorbell.
pub fn create(node: BorrowedFd<'_>, spec: &Spec<'_>) -> io::Result<Stream> {
    let mut arg: Create = zeroed();
    arg.block = spec.block;
    arg.flags = spec.flags;
    arg.buffers = spec.buffers;
    arg.buffer_bytes = spec.buffer_bytes;
    arg.frame_slots = spec.frame_slots;
    arg.job_slots = spec.job_slots;
    arg.job_bytes = spec.job_bytes;
    arg.pool_fd = spec.pool.map_or(-1, |pool| pool.as_raw_fd());
    arg.doorbell_fd = -1;
    arg.fd = -1;
    CREATE_REQ.call(node, &mut arg)?;
    // SAFETY: a successful create returns a new close-on-exec descriptor that nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(arg.fd) };
    let ring_bytes =
        usize::try_from(arg.ring_bytes).map_err(|_| io::Error::from_raw_os_error(libc::E2BIG))?;
    Ok(Stream {
        fd,
        ring_bytes,
        pool_addr: arg.pool_addr,
    })
}

/// Test only: makes `frames` frames, one every `period_us`, with the module's test source (the
/// module must be loaded with `test_stream_source`). A frame is published only when a buffer is
/// free, so a consumer that doesn't return buffers gets at most as many frames as it has.
pub fn test_source(stream: BorrowedFd<'_>, period_us: u32, frames: u32) -> io::Result<()> {
    let mut arg = TestSource { period_us, frames };
    TEST_SOURCE.call(stream, &mut arg).map(|_| ())
}

/// A reader the module holds: its slot, and the file that holds the registration. Closing the file
/// (an exit does too) frees the slot, and [`unregister`] does so at once.
#[derive(Debug)]
pub struct Registration {
    /// The reader slot the module gave the reader, to report with `Marks::set_slot`.
    pub slot: u32,
    /// The registration's file.
    pub file: OwnedFd,
}

/// Gives a stream's module a reader's marks page: a sealed memfd in the layout of
/// `shm_ring_holds::Marks` (its read-only descriptor, `Marks::page_fd`). The module holds every
/// buffer the page marks while the returned registration lives. `max_held` is what the reader may
/// hold at once, and `max_hold_ns` how long one hold may last; zero for no limit. A reader over
/// either is cut off by the module.
pub fn add_reader(
    stream: BorrowedFd<'_>,
    marks: BorrowedFd<'_>,
    max_held: u32,
    max_hold_ns: u64,
) -> io::Result<Registration> {
    let mut arg = Reader {
        fd: marks.as_raw_fd(),
        slot: 0,
        max_held,
        reader_fd: -1,
        max_hold_ns,
    };
    READER.call(stream, &mut arg)?;
    // SAFETY: a successful registration returns a new close-on-exec descriptor that nothing else owns.
    let file = unsafe { OwnedFd::from_raw_fd(arg.reader_fd) };
    Ok(Registration {
        slot: arg.slot,
        file,
    })
}

/// Frees a registration's slot now, through its file: the reader's marks stop holding at once. Fails
/// with `ENOENT` once the registration is gone.
pub fn unregister(registration: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: `_IO` takes no argument, so the kernel reads and writes nothing through the null pointer.
    unsafe { ioctl_ptr::<u8>(registration, UNREGISTER, std::ptr::null_mut()) }.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::Reader;

    /// The C struct is 24 bytes with the same offsets; `kernel/lemnos_stream.c` asserts its size.
    #[test]
    fn the_reader_struct_is_the_modules() {
        assert_eq!(std::mem::size_of::<Reader>(), 24);
        assert_eq!(std::mem::offset_of!(Reader, slot), 4);
        assert_eq!(std::mem::offset_of!(Reader, reader_fd), 12);
        assert_eq!(std::mem::offset_of!(Reader, max_hold_ns), 16);
    }
}
