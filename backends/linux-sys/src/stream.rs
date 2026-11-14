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
/// file out, and, when the module refuses the page's layout, which layout and both values out.
#[repr(C)]
#[derive(Clone, Copy)]
struct Reader {
    fd: i32,
    slot: u32,
    max_held: u32,
    reader_fd: i32,
    max_hold_ns: u64,
    refused: u32,
    layout: u32,
    found: u64,
    expected: u64,
}
pod!(Reader);

/// `struct lemnos_stream_reader_v0`: the first 24 bytes of [`Reader`], for a module from before the
/// refusal fields.
#[repr(C)]
#[derive(Clone, Copy)]
struct ReaderV0 {
    fd: i32,
    slot: u32,
    max_held: u32,
    reader_fd: i32,
    max_hold_ns: u64,
}
pod!(ReaderV0);

const READER: Ioctl<Reader> = Ioctl::new(iowr::<Reader>(TYPE, 0x87), "LEMNOS_STREAM_IOC_READER");
const READER_V0: Ioctl<ReaderV0> =
    Ioctl::new(iowr::<ReaderV0>(TYPE, 0x87), "LEMNOS_STREAM_IOC_READER_V0");

/// `LEMNOS_REFUSED_LAYOUT`: the refusal fields name a layout that is not the one the module reads.
const REFUSED_LAYOUT: u32 = 1;
/// `LEMNOS_LAYOUT_RING`.
const LAYOUT_RING: u32 = 0;
/// `LEMNOS_LAYOUT_MARKS`.
const LAYOUT_MARKS: u32 = 1;

/// `struct lemnos_stream_bell`: the consumer's doorbell, the eventfd and the page read-only.
#[repr(C)]
#[derive(Clone, Copy)]
struct Bell {
    eventfd: i32,
    page: i32,
}
pod!(Bell);

const BELL: Ioctl<Bell> = Ioctl::new(iow::<Bell>(TYPE, 0x89), "LEMNOS_STREAM_IOC_BELL");
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

/// The shared structure whose layout the module refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Structure {
    /// The frame ring's header.
    Ring,
    /// A reader's marks page.
    Marks,
}

/// A layout the module refused: the structure, the layout its memory says, and the one the module
/// reads. A page that never wrote its layout says 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutRefusal {
    /// The structure refused.
    pub which: Structure,
    /// The layout in the memory.
    pub found: u64,
    /// The layout the module reads.
    pub expected: u64,
}

/// Why [`add_reader`] failed.
#[derive(Debug)]
pub enum ReaderError {
    /// The module refused the marks page's layout (`EPROTO`, with the refusal fields).
    Layout(LayoutRefusal),
    /// Any other failure, including `EPROTO` from a module that gives no detail.
    Os(io::Error),
}

impl ReaderError {
    /// The `errno`, as an [`io::Error`] has it: `EPROTO` for a layout refusal.
    pub fn raw_os_error(&self) -> Option<i32> {
        match self {
            Self::Layout(_) => Some(libc::EPROTO),
            Self::Os(error) => error.raw_os_error(),
        }
    }
}

impl std::fmt::Display for ReaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Layout(LayoutRefusal {
                which,
                found,
                expected,
            }) => write!(
                f,
                "the {} is layout {found}; the module reads layout {expected}",
                match which {
                    Structure::Ring => "ring",
                    Structure::Marks => "marks page",
                }
            ),
            Self::Os(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ReaderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Layout(_) => None,
            Self::Os(error) => Some(error),
        }
    }
}

/// The error for a failed registration: a layout refusal when the module said `EPROTO` with one,
/// and the bare errno when it did not (a module from before the refusal fields, or any other
/// failure).
fn reader_error(error: io::Error, arg: &Reader) -> ReaderError {
    if error.raw_os_error() != Some(libc::EPROTO) || arg.refused != REFUSED_LAYOUT {
        return ReaderError::Os(error);
    }
    let which = match arg.layout {
        LAYOUT_RING => Structure::Ring,
        LAYOUT_MARKS => Structure::Marks,
        _ => return ReaderError::Os(error),
    };
    ReaderError::Layout(LayoutRefusal {
        which,
        found: arg.found,
        expected: arg.expected,
    })
}

/// Registers with `new`, and with `old` when the module does not know the request's current size
/// (`ENOTTY`): the slot and the file's descriptor number.
fn register(
    mut arg: Reader,
    new: impl FnOnce(&mut Reader) -> io::Result<()>,
    old: impl FnOnce(&mut ReaderV0) -> io::Result<()>,
) -> Result<(u32, i32), ReaderError> {
    match new(&mut arg) {
        Ok(()) => Ok((arg.slot, arg.reader_fd)),
        Err(error) if error.raw_os_error() == Some(libc::ENOTTY) => {
            let mut v0 = ReaderV0 {
                fd: arg.fd,
                slot: 0,
                max_held: arg.max_held,
                reader_fd: -1,
                max_hold_ns: arg.max_hold_ns,
            };
            old(&mut v0).map_err(ReaderError::Os)?;
            Ok((v0.slot, v0.reader_fd))
        }
        Err(error) => Err(reader_error(error, &arg)),
    }
}

/// Gives a stream's module a reader's marks page: a sealed memfd in the layout of
/// `shm_ring_holds::Marks` (its read-only descriptor, `Marks::page_fd`). The module holds every
/// buffer the page marks while the returned registration lives. `max_held` is what the reader may
/// hold at once, and `max_hold_ns` how long one hold may last; zero for no limit. A reader over
/// either is cut off by the module.
///
/// A page of another layout is [`ReaderError::Layout`], with the layout it says and the one the
/// module reads. A module from before the refusal fields answers `ENOTTY` to the request's current
/// size, so the request is made again at its old size; such a module refuses a layout with a bare
/// `EPROTO`, which is [`ReaderError::Os`].
///
/// # Errors
///
/// The refusal above, or the system's error.
pub fn add_reader(
    stream: BorrowedFd<'_>,
    marks: BorrowedFd<'_>,
    max_held: u32,
    max_hold_ns: u64,
) -> Result<Registration, ReaderError> {
    let arg = Reader {
        fd: marks.as_raw_fd(),
        slot: 0,
        max_held,
        reader_fd: -1,
        max_hold_ns,
        refused: 0,
        layout: 0,
        found: 0,
        expected: 0,
    };
    let (slot, reader_fd) = register(
        arg,
        |arg| READER.call(stream, arg).map(|_| ()),
        |arg| READER_V0.call(stream, arg).map(|_| ()),
    )?;
    // SAFETY: a successful registration returns a new close-on-exec descriptor that nothing else owns.
    let file = unsafe { OwnedFd::from_raw_fd(reader_fd) };
    Ok(Registration { slot, file })
}

/// What [`add_reader_at_old_size`] saw.
#[derive(Debug)]
pub struct OldSizeAnswer {
    /// The registration, or the system's error: a refused layout is a bare `EPROTO` here.
    pub result: io::Result<Registration>,
    /// Whether the module left the 24 bytes after the old-size argument as they were.
    pub tail_untouched: bool,
}

/// Test only: [`add_reader`]'s request at the size it had before the refusal fields (a caller built
/// against that header), in a buffer with a pattern after it, to see whether the module stays inside
/// the size the request names.
pub fn add_reader_at_old_size(
    stream: BorrowedFd<'_>,
    marks: BorrowedFd<'_>,
    max_held: u32,
    max_hold_ns: u64,
) -> OldSizeAnswer {
    const PATTERN: u64 = 0xa5a5_a5a5_a5a5_a5a5;
    #[repr(C)]
    struct Probe {
        head: ReaderV0,
        tail: [u64; 3],
    }
    let mut probe = Probe {
        head: ReaderV0 {
            fd: marks.as_raw_fd(),
            slot: 0,
            max_held,
            reader_fd: -1,
            max_hold_ns,
        },
        tail: [PATTERN; 3],
    };
    // SAFETY: the request names `ReaderV0`'s 24 bytes, the start of `probe`, which is live and
    // exclusive, and the pointer covers all of it, so the check below sees what the call did.
    let called = unsafe { ioctl_ptr(stream, READER_V0.nr(), &raw mut probe) };
    let tail_untouched = probe.tail == [PATTERN; 3];
    let result = called.map(|_| Registration {
        slot: probe.head.slot,
        // SAFETY: a successful registration returns a new close-on-exec descriptor that nothing else owns.
        file: unsafe { OwnedFd::from_raw_fd(probe.head.reader_fd) },
    });
    OldSizeAnswer {
        result,
        tail_untouched,
    }
}

/// Frees a registration's slot now, through its file: the reader's marks stop holding at once. Fails
/// with `ENOENT` once the registration is gone.
pub fn unregister(registration: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: `_IO` takes no argument, so the kernel reads and writes nothing through the null pointer.
    unsafe { ioctl_ptr::<u8>(registration, UNREGISTER, std::ptr::null_mut()) }.map(|_| ())
}

/// Gives a stream's module the consumer's doorbell: `eventfd` is the eventfd the consumer sleeps
/// on, and `page` the doorbell page, read-only (`shm_ring_doorbell::Doorbell::bell_fds`). The
/// module rings the eventfd after each publish that finds the consumer asleep, once per sleep.
/// Once per stream: a second call fails with `EBUSY`.
pub fn set_bell(
    stream: BorrowedFd<'_>,
    eventfd: BorrowedFd<'_>,
    page: BorrowedFd<'_>,
) -> io::Result<()> {
    let mut arg = Bell {
        eventfd: eventfd.as_raw_fd(),
        page: page.as_raw_fd(),
    };
    BELL.call(stream, &mut arg).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::{
        Bell, LAYOUT_MARKS, LAYOUT_RING, LayoutRefusal, REFUSED_LAYOUT, Reader, ReaderError,
        ReaderV0, Structure, iowr, reader_error, register,
    };
    use std::io;

    /// The C struct is 8 bytes; `kernel/lemnos_stream.c` asserts its size.
    #[test]
    fn the_bell_struct_is_the_modules() {
        assert_eq!(std::mem::size_of::<Bell>(), 8);
        assert_eq!(std::mem::offset_of!(Bell, page), 4);
    }

    /// The C struct is 48 bytes with the same offsets, and its first 24 are the old struct;
    /// `kernel/lemnos_stream.c` asserts both sizes.
    #[test]
    fn the_reader_struct_is_the_modules() {
        assert_eq!(std::mem::size_of::<Reader>(), 48);
        assert_eq!(std::mem::offset_of!(Reader, slot), 4);
        assert_eq!(std::mem::offset_of!(Reader, reader_fd), 12);
        assert_eq!(std::mem::offset_of!(Reader, max_hold_ns), 16);
        assert_eq!(std::mem::offset_of!(Reader, refused), 24);
        assert_eq!(std::mem::offset_of!(Reader, layout), 28);
        assert_eq!(std::mem::offset_of!(Reader, found), 32);
        assert_eq!(std::mem::offset_of!(Reader, expected), 40);
        assert_eq!(std::mem::size_of::<ReaderV0>(), 24);
        assert_eq!(std::mem::offset_of!(ReaderV0, max_hold_ns), 16);
    }

    /// Two sizes, one number: the request's size is bits 16 to 29, so only they differ.
    #[test]
    fn the_two_reader_requests_differ_in_size_only() {
        let new = iowr::<Reader>(b'L', 0x87);
        let old = iowr::<ReaderV0>(b'L', 0x87);
        assert_eq!(new ^ old, (48 ^ 24) << 16);
    }

    fn arg(refused: u32, layout: u32, found: u64, expected: u64) -> Reader {
        Reader {
            fd: 3,
            slot: 0,
            max_held: 0,
            reader_fd: -1,
            max_hold_ns: 0,
            refused,
            layout,
            found,
            expected,
        }
    }

    fn errno(code: i32) -> io::Error {
        io::Error::from_raw_os_error(code)
    }

    #[test]
    fn a_marks_layout_refusal_is_typed_with_its_structure_and_both_values() {
        let error = reader_error(
            errno(libc::EPROTO),
            &arg(REFUSED_LAYOUT, LAYOUT_MARKS, 1, 2),
        );
        match error {
            ReaderError::Layout(refusal) => assert_eq!(
                refusal,
                LayoutRefusal {
                    which: Structure::Marks,
                    found: 1,
                    expected: 2
                }
            ),
            ReaderError::Os(e) => panic!("the bare errno {e:?}"),
        }
        let ring = reader_error(errno(libc::EPROTO), &arg(REFUSED_LAYOUT, LAYOUT_RING, 7, 2));
        assert!(matches!(
            ring,
            ReaderError::Layout(LayoutRefusal {
                which: Structure::Ring,
                found: 7,
                expected: 2
            })
        ));
        assert_eq!(
            ring.to_string(),
            "the ring is layout 7; the module reads layout 2"
        );
    }

    #[test]
    fn a_bare_eproto_or_another_errno_stays_the_errno() {
        // A module from before the fields: EPROTO, the fields zero.
        let bare = reader_error(errno(libc::EPROTO), &arg(0, 0, 0, 0));
        assert!(matches!(&bare, ReaderError::Os(e) if e.raw_os_error() == Some(libc::EPROTO)));
        // Fields set but another errno: they say nothing about this failure.
        let other = reader_error(
            errno(libc::EINVAL),
            &arg(REFUSED_LAYOUT, LAYOUT_MARKS, 1, 2),
        );
        assert!(matches!(&other, ReaderError::Os(e) if e.raw_os_error() == Some(libc::EINVAL)));
        // A structure or refusal this build doesn't know is not invented into one.
        let unknown = reader_error(errno(libc::EPROTO), &arg(REFUSED_LAYOUT, 9, 1, 2));
        assert!(matches!(unknown, ReaderError::Os(_)));
        let future = reader_error(errno(libc::EPROTO), &arg(5, LAYOUT_MARKS, 1, 2));
        assert!(matches!(future, ReaderError::Os(_)));
        assert_eq!(bare.raw_os_error(), Some(libc::EPROTO));
    }

    #[test]
    fn registration_falls_back_to_the_old_size_on_enotty_only() {
        // An old module: the current size is unknown, the old size works.
        let got = register(
            arg(0, 0, 0, 0),
            |_| Err(errno(libc::ENOTTY)),
            |old| {
                old.slot = 5;
                old.reader_fd = 11;
                Ok(())
            },
        );
        assert!(matches!(got, Ok((5, 11))));
        // An old module that refuses the layout says only EPROTO.
        let refused = register(
            arg(0, 0, 0, 0),
            |_| Err(errno(libc::ENOTTY)),
            |_| Err(errno(libc::EPROTO)),
        );
        assert!(
            matches!(refused, Err(ReaderError::Os(e)) if e.raw_os_error() == Some(libc::EPROTO))
        );
        // The current size answered: no second request.
        let typed = register(
            arg(0, 0, 0, 0),
            |a| {
                *a = arg(REFUSED_LAYOUT, LAYOUT_MARKS, 1, 2);
                Err(errno(libc::EPROTO))
            },
            |_| panic!("the old size was asked after the module answered"),
        );
        assert!(matches!(typed, Err(ReaderError::Layout(_))));
    }
}
