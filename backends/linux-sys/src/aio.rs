//! Linux native AIO (`io_setup`, `io_submit`, `io_getevents`, `io_cancel`, `io_destroy`), for the
//! one thing it is needed for here: a read or a write with a timeout on a file that can be
//! neither polled nor read without blocking, such as a FunctionFS endpoint file.
//!
//! Provenance: the Linux uAPI headers are GPL-2.0 WITH Linux-syscall-note. The opcode numbers
//! and the `struct iocb` and `struct io_event` layouts below are interface facts transcribed
//! from `<linux/aio_abi.h>`; what cancelling and destroying do is from `fs/aio.c`. No kernel
//! code is copied.
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
        loop {
            let mut event = IoEvent::default();
            // SAFETY: `event` is one live `io_event`, matching the maximum of 1, and `timeout`
            // is null or points to the caller's live `timespec`.
            let got = unsafe {
                libc::syscall(
                    libc::SYS_io_getevents,
                    self.id,
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
}
