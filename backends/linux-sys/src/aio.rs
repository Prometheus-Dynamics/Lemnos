//! Linux native AIO (`io_setup`, `io_submit`, `io_getevents`, `io_cancel`, `io_destroy`), for the
//! one thing it is needed for here: a read or a write with a timeout on a file that can be
//! neither polled nor read without blocking, such as a FunctionFS endpoint file.
//!
//! Provenance: the Linux uAPI headers are GPL-2.0 WITH Linux-syscall-note. The opcode numbers
//! and the `struct iocb` and `struct io_event` layouts below are interface facts transcribed
//! from `<linux/aio_abi.h>`; what cancelling and destroying do is from `fs/aio.c`. No kernel
//! code is copied.
//!
//! Two forms. A [`Context`] runs a transfer on the caller's buffer and returns only when the
//! kernel has finished with it. A [`Queue`] owns the buffer, so a transfer can stay under way
//! after the call that began it has returned: a read that timed out is still there to finish,
//! and a write is the kernel's to send while the caller goes on.
//!
//! One transfer runs at a time, and [`Context::read`] and [`Context::write`] return only once
//! the kernel has finished with the caller's buffer: on a timeout the transfer is cancelled and
//! its completion waited for, without a limit, before the call returns. A kernel that never
//! completes a cancelled transfer would hold the call for ever; the context is destroyed
//! (which waits too) rather than let the buffer go while the kernel can still write to it.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::time::Duration;

/// `IOCB_CMD_PREAD`.
const PREAD: u16 = 0;
/// `IOCB_CMD_PWRITE`.
const PWRITE: u16 = 1;

/// The count of 1 the calls below take: one context slot, one block, one event.
const ONE: libc::c_long = 1;

/// `struct iocb`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct Iocb {
    data: u64,
    #[cfg(target_endian = "little")]
    key: u32,
    rw_flags: i32,
    #[cfg(target_endian = "big")]
    key: u32,
    opcode: u16,
    reqprio: i16,
    fildes: u32,
    buf: u64,
    nbytes: u64,
    offset: i64,
    reserved2: u64,
    flags: u32,
    resfd: u32,
}

/// `struct io_event`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct IoEvent {
    data: u64,
    obj: u64,
    res: i64,
    res2: i64,
}

fn check(ret: libc::c_long) -> io::Result<libc::c_long> {
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(ret)
}

/// Waits for the one completion context `id` can have: for as long as `timeout` says, or for
/// ever when it is null. `None` when the time ran out.
fn reap(id: libc::c_ulong, timeout: *mut libc::timespec) -> io::Result<Option<IoEvent>> {
    loop {
        let mut event = IoEvent::default();
        // SAFETY: `event` is one live `io_event`, matching the maximum of 1, and `timeout` is
        // null or points to the caller's live `timespec`.
        let got = unsafe {
            libc::syscall(
                libc::SYS_io_getevents,
                id,
                ONE,
                ONE,
                &raw mut event,
                timeout,
            )
        };
        match check(got) {
            Ok(0) => return Ok(None),
            Ok(_) => return Ok(Some(event)),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

fn timespec(timeout: Duration) -> libc::timespec {
    libc::timespec {
        tv_sec: libc::time_t::try_from(timeout.as_secs()).unwrap_or(libc::time_t::MAX),
        tv_nsec: timeout.subsec_nanos() as _,
    }
}

/// What a [`Queue`] is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flight {
    Idle,
    /// The kernel is reading into the buffer.
    Reading,
    /// The kernel is writing from the buffer.
    Writing,
    /// A read finished and `at..end` of the buffer is not taken yet.
    Read {
        at: usize,
        end: usize,
    },
}

/// One file's transfers, one at a time, on memory the queue owns, so a transfer can stay under
/// way after the call that began it returns. A queue goes one way: it reads or it writes.
///
/// - [`read`](Self::read) begins a read of the whole buffer if none is under way and waits
///   for it. When the time runs out the read stays under way and the next `read` finishes
///   it, so nothing that arrives is lost. What a finished read brought is handed out in as
///   many calls as the caller's buffers need.
/// - [`write`](Self::write) waits for the write before it, then copies the caller's bytes and
///   begins a write of them. It returns without waiting for that write.
///
/// The kernel has the buffer only between a transfer's submission and its completion, and
/// nothing here reads or writes it in between. Dropping the queue, [`cancel`](Self::cancel)
/// and every failure wait for the completion before the buffer is touched or freed.
#[derive(Debug)]
pub struct Queue {
    /// `aio_context_t`, or 0 when there is none.
    id: libc::c_ulong,
    buffer: Box<[u8]>,
    /// The block of the transfer under way. Boxed: `io_cancel` finds the transfer by this
    /// address.
    iocb: Box<Iocb>,
    /// Stamps each transfer so its completion can be told from any other.
    next: u64,
    /// How many bytes the transfer under way was begun with.
    submitted: usize,
    flight: Flight,
}

impl Queue {
    /// A queue whose transfers carry at most `capacity` bytes.
    pub fn new(capacity: usize) -> io::Result<Self> {
        Ok(Self {
            id: new_context()?,
            buffer: vec![0; capacity].into_boxed_slice(),
            iocb: Box::default(),
            next: 0,
            submitted: 0,
            flight: Flight::Idle,
        })
    }

    /// The most bytes one transfer carries.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.buffer.len()
    }

    /// Whether a transfer is with the kernel.
    #[must_use]
    pub fn under_way(&self) -> bool {
        matches!(self.flight, Flight::Reading | Flight::Writing)
    }

    /// Takes bytes read from `fd` into `data`, waiting at most `timeout` for a read to finish,
    /// then failing with [`io::ErrorKind::TimedOut`] and leaving the read under way. Returns
    /// how many bytes were taken: all a finished read brought, or as much of it as `data`
    /// holds, the rest staying for the next call. A read that brought nothing (a zero-length
    /// packet) returns 0.
    ///
    /// A read that failed is reported once, with its error, and the next call begins another.
    /// `fd` must be the same file for as long as a read is under way or has bytes left.
    pub fn read(
        &mut self,
        fd: BorrowedFd<'_>,
        data: &mut [u8],
        timeout: Duration,
    ) -> io::Result<usize> {
        loop {
            match self.flight {
                Flight::Read { at, end } => {
                    let n = data.len().min(end - at);
                    data[..n].copy_from_slice(&self.buffer[at..at + n]);
                    self.flight = if at + n == end {
                        Flight::Idle
                    } else {
                        Flight::Read { at: at + n, end }
                    };
                    return Ok(n);
                }
                Flight::Writing => return Err(io::ErrorKind::InvalidInput.into()),
                Flight::Idle => {
                    self.submit(fd, PREAD, self.buffer.len())?;
                    self.flight = Flight::Reading;
                }
                Flight::Reading => {
                    let end = self.finish(timeout)?;
                    self.flight = Flight::Read { at: 0, end };
                }
            }
        }
    }

    /// Begins a write of `data` to `fd`, after waiting at most `timeout` for the write before
    /// it to finish; [`io::ErrorKind::TimedOut`] when it has not, and nothing of `data` is
    /// taken. Returns how many bytes were taken: at most [`capacity`](Self::capacity). The
    /// write itself is not waited for.
    ///
    /// When the write before it failed, this reports that error and takes nothing; the next
    /// call begins anew.
    pub fn write(
        &mut self,
        fd: BorrowedFd<'_>,
        data: &[u8],
        timeout: Duration,
    ) -> io::Result<usize> {
        match self.flight {
            Flight::Reading | Flight::Read { .. } => {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            Flight::Writing => {
                self.finish(timeout)?;
            }
            Flight::Idle => {}
        }
        let n = data.len().min(self.buffer.len());
        self.buffer[..n].copy_from_slice(&data[..n]);
        self.submit(fd, PWRITE, n)?;
        self.flight = Flight::Writing;
        Ok(n)
    }

    /// Gives up the transfer under way, waiting until the kernel has let go of the buffer, and
    /// forgets bytes read and not taken. The queue is idle afterwards.
    pub fn cancel(&mut self) {
        if self.under_way() {
            let mut ignored = IoEvent::default();
            // SAFETY: `iocb` is the live block of the transfer under way, at the address it
            // was submitted from, and `ignored` a live event the kernel may write. The result
            // is not used: cancelled, already complete or not cancellable, the completion is
            // waited for next.
            unsafe {
                libc::syscall(
                    libc::SYS_io_cancel,
                    self.id,
                    &raw mut *self.iocb,
                    &raw mut ignored,
                )
            };
            let own = matches!(
                reap(self.id, std::ptr::null_mut()),
                Ok(Some(event)) if event.data == self.next
            );
            if !own {
                self.renew();
            }
        }
        self.flight = Flight::Idle;
    }

    /// Hands the kernel a transfer of the first `len` bytes of the buffer.
    fn submit(&mut self, fd: BorrowedFd<'_>, opcode: u16, len: usize) -> io::Result<()> {
        if self.id == 0 {
            self.id = new_context()?;
        }
        self.next = self.next.wrapping_add(1);
        *self.iocb = Iocb {
            data: self.next,
            opcode,
            fildes: fd.as_raw_fd() as u32,
            buf: self.buffer.as_mut_ptr() as u64,
            nbytes: len as u64,
            ..Iocb::default()
        };
        let mut list = [&raw mut *self.iocb];
        // SAFETY: `list` holds one pointer to the live boxed `iocb`, matching the count 1. The
        // buffer it names is this queue's own, `len` is within it, and it is neither read,
        // written nor freed until the completion has been reaped: `flight` says a transfer is
        // under way from here until `finish`, `cancel` or `renew` has seen it end.
        let submitted =
            unsafe { libc::syscall(libc::SYS_io_submit, self.id, ONE, list.as_mut_ptr()) };
        if check(submitted)? != 1 {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        self.submitted = len;
        Ok(())
    }

    /// Waits at most `timeout` for the transfer under way and returns how many bytes it
    /// moved. The queue is idle after anything but a timeout.
    fn finish(&mut self, timeout: Duration) -> io::Result<usize> {
        let mut wait = timespec(timeout);
        let event = match reap(self.id, &raw mut wait) {
            Ok(Some(event)) => event,
            Ok(None) => return Err(io::ErrorKind::TimedOut.into()),
            Err(error) => {
                self.renew();
                return Err(error);
            }
        };
        if event.data != self.next {
            self.renew();
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        self.flight = Flight::Idle;
        match usize::try_from(event.res) {
            Ok(n) if n <= self.submitted => Ok(n),
            Ok(_) => Err(io::ErrorKind::InvalidData.into()),
            Err(_) => Err(io::Error::from_raw_os_error(
                i32::try_from(event.res.unsigned_abs()).unwrap_or(libc::EIO),
            )),
        }
    }

    /// Destroys the context, which cancels what is under way and waits until the kernel has
    /// finished with the buffer. The next transfer makes a new context.
    fn renew(&mut self) {
        if self.id != 0 {
            // SAFETY: `id` is a context from `io_setup` that has not been destroyed.
            unsafe { libc::syscall(libc::SYS_io_destroy, self.id) };
            self.id = 0;
        }
        self.flight = Flight::Idle;
    }
}

impl Drop for Queue {
    /// Waits for the kernel to let go of the buffer before it is freed.
    fn drop(&mut self) {
        self.renew();
    }
}

/// A new context with room for one transfer.
fn new_context() -> io::Result<libc::c_ulong> {
    let mut id: libc::c_ulong = 0;
    // SAFETY: `io_setup` writes one `aio_context_t` through the pointer, which is to a live
    // one that is 0 as the call requires.
    check(unsafe { libc::syscall(libc::SYS_io_setup, ONE, &raw mut id) })?;
    Ok(id)
}

/// An AIO context that runs one transfer at a time.
#[derive(Debug)]
pub struct Context {
    /// `aio_context_t`, or 0 once the context has been destroyed.
    id: libc::c_ulong,
    /// Stamps each transfer so its completion can be told from any other.
    next: u64,
}

impl Context {
    /// A new context with room for the one transfer.
    pub fn new() -> io::Result<Self> {
        let mut id: libc::c_ulong = 0;
        // SAFETY: `io_setup` writes one `aio_context_t` through the pointer, which is to a
        // live one that is 0 as the call requires.
        check(unsafe { libc::syscall(libc::SYS_io_setup, ONE, &raw mut id) })?;
        Ok(Self { id, next: 0 })
    }

    /// Reads from `fd` into `data`, giving up after `timeout` with
    /// [`io::ErrorKind::TimedOut`]. Bytes of a transfer that was given up are lost.
    pub fn read(
        &mut self,
        fd: BorrowedFd<'_>,
        data: &mut [u8],
        timeout: Duration,
    ) -> io::Result<usize> {
        // SAFETY: `data` is writable for its length and is borrowed until `run` returns, which
        // it does only once the kernel has finished with it.
        unsafe { self.run(fd, PREAD, data.as_mut_ptr(), data.len(), timeout) }
    }

    /// Writes `data` to `fd`, giving up after `timeout` with [`io::ErrorKind::TimedOut`].
    pub fn write(
        &mut self,
        fd: BorrowedFd<'_>,
        data: &[u8],
        timeout: Duration,
    ) -> io::Result<usize> {
        // SAFETY: `data` is readable for its length and borrowed until `run` returns; a write
        // (`IOCB_CMD_PWRITE`) only reads through the pointer, so the cast to `*mut` writes
        // nothing.
        unsafe { self.run(fd, PWRITE, data.as_ptr().cast_mut(), data.len(), timeout) }
    }

    /// Runs one transfer to its end or to `timeout`.
    ///
    /// # Safety
    ///
    /// `buf` must be valid for `len` bytes of the access `opcode` makes (writes for `PREAD`,
    /// reads for `PWRITE`) until this returns.
    unsafe fn run(
        &mut self,
        fd: BorrowedFd<'_>,
        opcode: u16,
        buf: *mut u8,
        len: usize,
        timeout: Duration,
    ) -> io::Result<usize> {
        if self.id == 0 {
            return Err(io::Error::from_raw_os_error(libc::EBADF));
        }
        self.next = self.next.wrapping_add(1);
        let mut iocb = Iocb {
            data: self.next,
            opcode,
            fildes: fd.as_raw_fd() as u32,
            buf: buf as u64,
            nbytes: len as u64,
            ..Iocb::default()
        };
        let mut list = [&raw mut iocb];
        // SAFETY: `list` holds one pointer to a live `iocb`, matching the count 1. The buffer
        // it names is valid for the transfer by this function's contract, and every path below
        // waits for the completion before returning, so it is not used after that.
        let submitted =
            unsafe { libc::syscall(libc::SYS_io_submit, self.id, ONE, list.as_mut_ptr()) };
        if check(submitted)? != 1 {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        let mut wait = libc::timespec {
            tv_sec: libc::time_t::try_from(timeout.as_secs()).unwrap_or(libc::time_t::MAX),
            tv_nsec: timeout.subsec_nanos() as _,
        };
        let (event, timed_out) = match self.reap(&raw mut wait) {
            Ok(Some(event)) => (event, false),
            Ok(None) => {
                let mut ignored = IoEvent::default();
                // SAFETY: `iocb` is the live block submitted above and `ignored` a live event
                // the kernel may write. The result is not used: whether the transfer was
                // cancelled, had already completed or can't be cancelled, its completion is
                // waited for next.
                unsafe {
                    libc::syscall(
                        libc::SYS_io_cancel,
                        self.id,
                        &raw mut iocb,
                        &raw mut ignored,
                    )
                };
                match self.reap(std::ptr::null_mut()) {
                    Ok(Some(event)) => (event, true),
                    Ok(None) | Err(_) => return Err(self.destroy()),
                }
            }
            Err(_) => return Err(self.destroy()),
        };
        if event.data != self.next {
            return Err(self.destroy());
        }
        match usize::try_from(event.res) {
            Ok(n) if n <= len => Ok(n),
            Ok(_) => Err(io::ErrorKind::InvalidData.into()),
            Err(_) if timed_out => Err(io::ErrorKind::TimedOut.into()),
            Err(_) => Err(io::Error::from_raw_os_error(
                i32::try_from(event.res.unsigned_abs()).unwrap_or(libc::EIO),
            )),
        }
    }

    /// Waits for the one completion there can be: for as long as `timeout` says, or for ever
    /// when it is null. `None` when the time ran out.
    fn reap(&mut self, timeout: *mut libc::timespec) -> io::Result<Option<IoEvent>> {
        reap(self.id, timeout)
    }

    /// Destroys the context, which cancels what is outstanding and waits until the kernel has
    /// finished with it. Every later transfer fails. Returns the error to report.
    fn destroy(&mut self) -> io::Error {
        if self.id != 0 {
            // SAFETY: `id` is a context from `io_setup` that has not been destroyed.
            unsafe { libc::syscall(libc::SYS_io_destroy, self.id) };
            self.id = 0;
        }
        io::Error::from_raw_os_error(libc::EIO)
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        self.destroy();
    }
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write as _;
    use std::os::fd::AsFd;

    use super::*;

    const WAIT: Duration = Duration::from_millis(200);

    #[test]
    fn layouts_match_the_kernel_header() {
        assert_eq!(size_of::<Iocb>(), 64);
        assert_eq!(std::mem::offset_of!(Iocb, opcode), 16);
        assert_eq!(std::mem::offset_of!(Iocb, reqprio), 18);
        assert_eq!(std::mem::offset_of!(Iocb, fildes), 20);
        assert_eq!(std::mem::offset_of!(Iocb, buf), 24);
        assert_eq!(std::mem::offset_of!(Iocb, nbytes), 32);
        assert_eq!(std::mem::offset_of!(Iocb, offset), 40);
        assert_eq!(std::mem::offset_of!(Iocb, flags), 56);
        assert_eq!(std::mem::offset_of!(Iocb, resfd), 60);
        #[cfg(target_endian = "little")]
        assert_eq!(
            (
                std::mem::offset_of!(Iocb, key),
                std::mem::offset_of!(Iocb, rw_flags)
            ),
            (8, 12)
        );
        assert_eq!(size_of::<IoEvent>(), 32);
        assert_eq!(std::mem::offset_of!(IoEvent, res), 16);
        assert_eq!((PREAD, PWRITE), (0, 1));
    }

    fn temp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("lemnos-aio-{}-{name}", std::process::id()))
    }

    #[test]
    fn a_file_is_written_and_read_back() {
        let path = temp("file");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        let mut aio = Context::new().unwrap();
        assert_eq!(aio.write(file.as_fd(), b"hello", WAIT).unwrap(), 5);
        let mut data = [0u8; 8];
        assert_eq!(aio.read(file.as_fd(), &mut data, WAIT).unwrap(), 5);
        assert_eq!(&data[..5], b"hello");
        assert_eq!(aio.read(file.as_fd(), &mut [], WAIT).unwrap(), 0);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_failed_transfer_reports_the_errno_and_the_context_carries_on() {
        let path = temp("readonly");
        std::fs::write(&path, b"abc").unwrap();
        let file = OpenOptions::new().read(true).open(&path).unwrap();
        let mut aio = Context::new().unwrap();
        let error = aio.write(file.as_fd(), b"x", WAIT).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EBADF));
        let mut data = [0u8; 3];
        assert_eq!(aio.read(file.as_fd(), &mut data, WAIT).unwrap(), 3);
        assert_eq!(&data, b"abc");
        std::fs::remove_file(path).unwrap();
    }

    // No file a test can open here completes later than `io_submit` returns: regular files and
    // pipes run in place. The path that times out, cancels and waits is for a board run.
    #[test]
    fn a_pipe_with_data_is_read_in_place() {
        let (reader, mut writer) = std::io::pipe().unwrap();
        let mut aio = Context::new().unwrap();
        writer.write_all(b"ab").unwrap();
        let mut data = [0u8; 4];
        assert_eq!(aio.read(reader.as_fd(), &mut data, WAIT).unwrap(), 2);
        assert_eq!(&data[..2], b"ab");
        writer.write_all(b"c").unwrap();
        assert_eq!(aio.read(reader.as_fd(), &mut data, WAIT).unwrap(), 1);
    }

    #[test]
    fn a_destroyed_context_refuses_transfers() {
        let mut aio = Context::new().unwrap();
        aio.destroy();
        let (reader, _writer) = std::io::pipe().unwrap();
        let error = aio.read(reader.as_fd(), &mut [0u8; 1], WAIT).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EBADF));
    }

    fn scratch_file(name: &str) -> (std::path::PathBuf, std::fs::File) {
        let path = temp(name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        (path, file)
    }

    #[test]
    fn a_queue_writes_from_its_own_memory_and_reports_the_write_with_the_next() {
        let (path, file) = scratch_file("queue-write");
        let mut queue = Queue::new(4).unwrap();
        assert_eq!(queue.capacity(), 4);
        // At most the capacity is taken, and the caller hands over the rest.
        assert_eq!(queue.write(file.as_fd(), b"hello", WAIT).unwrap(), 4);
        assert!(queue.under_way());
        assert_eq!(queue.write(file.as_fd(), b"", WAIT).unwrap(), 0);
        assert_eq!(std::fs::read(&path).unwrap(), b"hell");
        // A queue goes one way.
        let error = queue.read(file.as_fd(), &mut [0u8; 4], WAIT).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_failed_write_is_reported_by_the_next_which_takes_nothing() {
        // A pipe nobody reads fails the write once the kernel runs it, not when it is handed
        // over.
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let mut queue = Queue::new(8).unwrap();
        assert_eq!(queue.write(writer.as_fd(), b"x", WAIT).unwrap(), 1);
        let error = queue.write(writer.as_fd(), b"y", WAIT).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EPIPE));
        assert!(
            !queue.under_way(),
            "the failed write is over and nothing was begun"
        );
        // A write the kernel refuses outright is not under way either.
        let path = temp("queue-readonly");
        std::fs::write(&path, b"abc").unwrap();
        let file = OpenOptions::new().read(true).open(&path).unwrap();
        let error = queue.write(file.as_fd(), b"z", WAIT).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EBADF));
        assert!(!queue.under_way());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_queue_hands_out_what_a_read_brought_in_as_many_calls_as_it_takes() {
        let (reader, mut writer) = std::io::pipe().unwrap();
        let mut queue = Queue::new(8).unwrap();
        writer.write_all(b"abcde").unwrap();
        let mut data = [0u8; 2];
        assert_eq!(queue.read(reader.as_fd(), &mut data, WAIT).unwrap(), 2);
        assert_eq!(&data, b"ab");
        // What is left of that read comes before anything newer.
        writer.write_all(b"XY").unwrap();
        assert_eq!(queue.read(reader.as_fd(), &mut data, WAIT).unwrap(), 2);
        assert_eq!(&data, b"cd");
        let mut rest = [0u8; 8];
        assert_eq!(queue.read(reader.as_fd(), &mut rest, WAIT).unwrap(), 1);
        assert_eq!(rest[0], b'e');
        assert_eq!(queue.read(reader.as_fd(), &mut rest, WAIT).unwrap(), 2);
        assert_eq!(&rest[..2], b"XY");
    }

    #[test]
    fn a_read_that_fails_is_reported_once_and_the_next_begins_another() {
        let (reader, mut writer) = std::io::pipe().unwrap();
        // An empty pipe that doesn't block answers "try again" in place.
        nonblocking(reader.as_fd());
        let mut queue = Queue::new(8).unwrap();
        let mut data = [0u8; 8];
        let error = queue.read(reader.as_fd(), &mut data, WAIT).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EAGAIN));
        assert!(!queue.under_way());
        writer.write_all(b"ok").unwrap();
        assert_eq!(queue.read(reader.as_fd(), &mut data, WAIT).unwrap(), 2);
        assert_eq!(&data[..2], b"ok");
    }

    /// Sets `O_NONBLOCK` on `fd`.
    fn nonblocking(fd: BorrowedFd<'_>) {
        // SAFETY: `F_SETFL` on a live descriptor takes an integer and touches no memory.
        let done = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
        assert_eq!(done, 0);
    }

    #[test]
    fn cancelling_an_idle_queue_forgets_what_was_read_and_not_taken() {
        let (reader, mut writer) = std::io::pipe().unwrap();
        let mut queue = Queue::new(8).unwrap();
        writer.write_all(b"old").unwrap();
        assert_eq!(queue.read(reader.as_fd(), &mut [0u8; 1], WAIT).unwrap(), 1);
        queue.cancel();
        writer.write_all(b"new").unwrap();
        let mut data = [0u8; 8];
        assert_eq!(queue.read(reader.as_fd(), &mut data, WAIT).unwrap(), 3);
        assert_eq!(&data[..3], b"new");
    }

    // No file a test can open here completes later than `io_submit` returns, so a transfer
    // still under way when its call returns, and cancelling one, are for a board run.
}
