//! Linux native AIO (`io_setup`, `io_submit`, `io_getevents`, `io_cancel`, `io_destroy`), for the
//! one thing it is needed for here: a read or a write with a timeout on a file that can be
//! neither polled nor read without blocking, such as a FunctionFS endpoint file.
//!
//! Provenance: the Linux uAPI headers are GPL-2.0 WITH Linux-syscall-note. The opcode numbers
//! and the `struct iocb` and `struct io_event` layouts below are interface facts transcribed
//! from `<linux/aio_abi.h>`; what cancelling and destroying do is from `fs/aio.c`. No kernel
//! code is copied.
//!
//! Three forms. A [`Context`] runs a transfer on the caller's buffer and returns only when the
//! kernel has finished with it. A [`Queue`] owns the buffer, so a transfer can stay under way
//! after the call that began it has returned: a read that timed out is still there to finish,
//! and a write is the kernel's to send while the caller goes on. A [`Ring`] keeps several
//! transfers of its own memory with the kernel at once, for an isochronous endpoint, where
//! a transfer must be waiting at every service interval.
//!
//! In a context or a queue one transfer runs at a time, and [`Context::read`] and [`Context::write`] return only once
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
    /// The read asks `fd` for as many bytes as `data` holds, at most the capacity. It must ask
    /// for that much and no more: a FunctionFS endpoint completes a read only when it is full or
    /// a short packet arrives, so a larger read waits for packets the host never sends.
    ///
    /// A read that failed is reported once, with its error, and the next call begins another.
    /// `fd` must be the same file for as long as a read is under way or has bytes left.
    pub fn read(
        &mut self,
        fd: BorrowedFd<'_>,
        data: &mut [u8],
        timeout: Duration,
    ) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
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
                    let asked = data.len().min(self.buffer.len());
                    self.submit(fd, PREAD, asked)?;
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
    context_for(1)
}

/// A new context with room for `transfers` at once.
fn context_for(transfers: usize) -> io::Result<libc::c_ulong> {
    let mut id: libc::c_ulong = 0;
    let transfers = libc::c_long::try_from(transfers).map_err(|_| io::ErrorKind::InvalidInput)?;
    // SAFETY: `io_setup` writes one `aio_context_t` through the pointer, which is to a live
    // one that is 0 as the call requires.
    check(unsafe { libc::syscall(libc::SYS_io_setup, transfers, &raw mut id) })?;
    Ok(id)
}

/// The most transfers a [`Ring`] keeps with the kernel.
pub const MAX_RING: usize = 16;

/// Where one of a [`Ring`]'s transfers is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// Not with the kernel, and nothing in it to hand out.
    Idle,
    /// With the kernel, stamped so its completion can be told.
    Busy(u64),
    /// Back from the kernel, stamped as it was, with the kernel's result: bytes moved or a
    /// negative errno.
    Done(u64, i64),
}

/// Several transfers with one file at once, each on memory of its own the ring owns, so the
/// kernel always has the next ready while the caller works on the last. A ring goes one way:
/// it reads or it writes, as its first call says.
///
/// - [`write`](Self::write) copies the caller's bytes into a free transfer and hands it to
///   the kernel, waiting at most its timeout for one to be free. It never waits for the write
///   itself, and how a write ended is not reported: a stream of writes goes on past one that
///   failed.
/// - [`read`](Self::read) keeps every transfer with the kernel as a read, and hands out the
///   oldest once it is done, handing that transfer back to the kernel at once.
///
/// The kernel has a transfer's memory only between its submission and its completion, and
/// nothing here reads or writes it in between; each transfer's memory is its own allocation,
/// so touching one never touches another. Dropping the ring, [`cancel`](Self::cancel) and
/// every failure the ring can't account for destroy the context, which waits until the
/// kernel has let go of all of them. Nothing allocates after [`new`](Self::new), but a
/// context destroyed is made again by the next transfer.
#[derive(Debug)]
pub struct Ring {
    /// `aio_context_t`, or 0 when there is none.
    id: libc::c_ulong,
    /// Each transfer's memory.
    buffers: Box<[Box<[u8]>]>,
    /// Each transfer's block. Boxed apart from the ring so a block's address stays put.
    blocks: Box<[Iocb]>,
    slots: [Slot; MAX_RING],
    /// The stamp of the next transfer: stamps count up, so the oldest has the lowest.
    next: u64,
    /// Whether the ring reads, once its first call has said.
    reading: Option<bool>,
}

impl Ring {
    /// A ring of `transfers` transfers, 1 to [`MAX_RING`], of at most `bytes` each.
    pub fn new(transfers: usize, bytes: usize) -> io::Result<Self> {
        if !(1..=MAX_RING).contains(&transfers) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(Self {
            id: context_for(transfers)?,
            buffers: (0..transfers)
                .map(|_| vec![0; bytes].into_boxed_slice())
                .collect(),
            blocks: vec![Iocb::default(); transfers].into_boxed_slice(),
            slots: [Slot::Idle; MAX_RING],
            next: 0,
            reading: None,
        })
    }

    /// The most bytes one transfer carries.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.buffers.first().map_or(0, |buffer| buffer.len())
    }

    /// How many transfers the ring has.
    #[must_use]
    pub fn transfers(&self) -> usize {
        self.buffers.len()
    }

    /// How many transfers are with the kernel.
    #[must_use]
    pub fn under_way(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| matches!(slot, Slot::Busy(_)))
            .count()
    }

    /// Hands `data` to the kernel to write to `fd`, in a free transfer, waiting at most
    /// `timeout` for one to be free: [`io::ErrorKind::TimedOut`] when none is, and nothing is
    /// taken. `data` must fit one transfer, else [`io::ErrorKind::InvalidInput`]. Returns
    /// `data.len()`. `fd` must be the same file for as long as a write is under way.
    pub fn write(
        &mut self,
        fd: BorrowedFd<'_>,
        data: &[u8],
        timeout: Duration,
    ) -> io::Result<usize> {
        self.going(false)?;
        if data.len() > self.capacity() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        if self.under_way() != 0 {
            self.harvest(Some(Duration::ZERO), 0)?;
        }
        let at = match self.free() {
            Some(at) => at,
            None => {
                self.harvest(Some(timeout), 1)?;
                self.free().ok_or(io::ErrorKind::TimedOut)?
            }
        };
        self.buffers[at][..data.len()].copy_from_slice(data);
        self.submit(at, fd, PWRITE, data.len())?;
        Ok(data.len())
    }

    /// Takes the oldest read from `fd` into `data`, waiting at most `timeout` for it to be
    /// done: [`io::ErrorKind::TimedOut`] when it is not, and every read stays under way.
    /// Returns how many bytes it brought, at most `data.len()`; bytes past that are lost, so
    /// `data` should hold a whole transfer. A read that failed is reported with its error.
    /// Either way the transfer goes back to the kernel as a read before this returns, and so
    /// does every transfer not with it. `fd` must be the same file for as long as a read is
    /// under way.
    pub fn read(
        &mut self,
        fd: BorrowedFd<'_>,
        data: &mut [u8],
        timeout: Duration,
    ) -> io::Result<usize> {
        self.going(true)?;
        self.prime(fd)?;
        let deadline = std::time::Instant::now() + timeout;
        let at = loop {
            let oldest = self.oldest().ok_or(io::ErrorKind::TimedOut)?;
            if matches!(self.slots[oldest], Slot::Done(..)) {
                break oldest;
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if self.harvest(Some(left), 1)? == 0 {
                return Err(io::ErrorKind::TimedOut.into());
            }
        };
        let Slot::Done(_, result) = self.slots[at] else {
            unreachable!("the loop above stops at a done transfer");
        };
        let taken = match usize::try_from(result) {
            Ok(n) if n <= self.capacity() => {
                let n = n.min(data.len());
                data[..n].copy_from_slice(&self.buffers[at][..n]);
                Ok(n)
            }
            Ok(_) => Err(io::ErrorKind::InvalidData.into()),
            Err(_) => Err(io::Error::from_raw_os_error(
                i32::try_from(result.unsigned_abs()).unwrap_or(libc::EIO),
            )),
        };
        self.slots[at] = Slot::Idle;
        // Back to the kernel at once. One that can't go now goes with the next call.
        let _ = self.submit(at, fd, PREAD, self.capacity());
        taken
    }

    /// Gives up every transfer, waiting until the kernel has let go of all of them, and
    /// forgets reads not handed out. The ring is idle afterwards, and may go either way again.
    pub fn cancel(&mut self) {
        self.renew();
        self.reading = None;
    }

    /// Fails with [`io::ErrorKind::InvalidInput`] if the ring already goes the other way.
    fn going(&mut self, reading: bool) -> io::Result<()> {
        match self.reading {
            Some(going) if going != reading => Err(io::ErrorKind::InvalidInput.into()),
            _ => {
                self.reading = Some(reading);
                Ok(())
            }
        }
    }

    fn free(&self) -> Option<usize> {
        (0..self.transfers()).find(|&at| self.slots[at] == Slot::Idle)
    }

    /// The transfer submitted longest ago that is not handed out yet.
    fn oldest(&self) -> Option<usize> {
        (0..self.transfers())
            .filter_map(|at| match self.slots[at] {
                Slot::Busy(stamp) | Slot::Done(stamp, _) => Some((stamp, at)),
                Slot::Idle => None,
            })
            .min()
            .map(|(_, at)| at)
    }

    /// Hands every idle transfer to the kernel as a read. An error is reported once no
    /// transfer at all is with the kernel or done; otherwise the reads there are go on.
    fn prime(&mut self, fd: BorrowedFd<'_>) -> io::Result<()> {
        let capacity = self.capacity();
        for at in 0..self.transfers() {
            if self.slots[at] == Slot::Idle
                && let Err(error) = self.submit(at, fd, PREAD, capacity)
            {
                if self.oldest().is_none() {
                    return Err(error);
                }
                break;
            }
        }
        Ok(())
    }

    /// Hands the kernel transfer `at` of the first `len` bytes of its memory.
    fn submit(&mut self, at: usize, fd: BorrowedFd<'_>, opcode: u16, len: usize) -> io::Result<()> {
        if self.id == 0 {
            self.id = context_for(self.transfers())?;
        }
        self.next = self.next.wrapping_add(1);
        self.blocks[at] = Iocb {
            data: self.next,
            opcode,
            fildes: fd.as_raw_fd() as u32,
            buf: self.buffers[at].as_mut_ptr() as u64,
            nbytes: len as u64,
            ..Iocb::default()
        };
        let mut list = [&raw mut self.blocks[at]];
        // SAFETY: `list` holds one pointer to a live block of the ring, matching the count 1.
        // The memory it names is transfer `at`'s own, `len` is within it, and it is neither
        // read, written nor freed until the completion has been reaped: the slot says
        // `Busy` from here until `harvest` has seen it end, or `renew` has destroyed the
        // context, which waits for it.
        let submitted =
            unsafe { libc::syscall(libc::SYS_io_submit, self.id, ONE, list.as_mut_ptr()) };
        if check(submitted)? != 1 {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        self.slots[at] = Slot::Busy(self.next);
        Ok(())
    }

    /// Takes the completions there are, waiting at most `timeout` (for ever when `None`) for
    /// at least `least` of them. A write's completion frees its transfer; a read's leaves it
    /// done, to be handed out. Returns how many came. A completion the ring did not ask for,
    /// or a failure to wait, destroys the context.
    fn harvest(&mut self, timeout: Option<Duration>, least: usize) -> io::Result<usize> {
        if self.id == 0 {
            return Ok(0);
        }
        let mut events = [IoEvent::default(); MAX_RING];
        let mut wait = timeout.map(timespec);
        let wait = wait
            .as_mut()
            .map_or(std::ptr::null_mut(), |wait| &raw mut *wait);
        let got = loop {
            // SAFETY: `events` is `MAX_RING` live `io_event`s, matching the maximum, and
            // `wait` is null or points to the live `timespec` above.
            let got = unsafe {
                libc::syscall(
                    libc::SYS_io_getevents,
                    self.id,
                    least as libc::c_long,
                    MAX_RING as libc::c_long,
                    events.as_mut_ptr(),
                    wait,
                )
            };
            match check(got) {
                Ok(got) => break got as usize,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    self.renew();
                    return Err(error);
                }
            }
        };
        let reading = self.reading == Some(true);
        for event in &events[..got] {
            let at = (0..self.transfers()).find(|&at| self.slots[at] == Slot::Busy(event.data));
            let Some(at) = at else {
                self.renew();
                return Err(io::Error::from_raw_os_error(libc::EIO));
            };
            self.slots[at] = if reading {
                Slot::Done(event.data, event.res)
            } else {
                Slot::Idle
            };
        }
        Ok(got)
    }

    /// Destroys the context, which cancels every transfer and waits until the kernel has let
    /// go of their memory. The next transfer makes a new context.
    fn renew(&mut self) {
        if self.id != 0 {
            // SAFETY: `id` is a context from `io_setup` that has not been destroyed.
            unsafe { libc::syscall(libc::SYS_io_destroy, self.id) };
            self.id = 0;
        }
        self.slots = [Slot::Idle; MAX_RING];
    }
}

impl Drop for Ring {
    /// Waits for the kernel to let go of every transfer's memory before it is freed.
    fn drop(&mut self) {
        self.renew();
    }
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
    fn a_read_asks_for_what_the_caller_holds_and_no_more() {
        // A pipe gives what it holds at once, so bytes left in it show how much a read asked for.
        let (reader, mut writer) = std::io::pipe().unwrap();
        let mut queue = Queue::new(64 * 1024).unwrap();
        writer.write_all(&[1u8; 100]).unwrap();
        let mut data = [0u8; 16];
        assert_eq!(queue.read(reader.as_fd(), &mut data, WAIT).unwrap(), 16);
        // 84 bytes are in the pipe; a read that asked for 200 gets those and the 50 written now.
        writer.write_all(&[2u8; 50]).unwrap();
        let mut more = [0u8; 200];
        assert_eq!(queue.read(reader.as_fd(), &mut more, WAIT).unwrap(), 134);
        assert!(more[..84].iter().all(|&b| b == 1) && more[84..134].iter().all(|&b| b == 2));
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
    fn a_queue_hands_out_what_each_read_brings_in_as_many_calls_as_it_takes() {
        // Each read asks for what its caller holds, so bytes the caller leaves stay in the pipe
        // and the next read gets them, in order.
        let (reader, mut writer) = std::io::pipe().unwrap();
        let mut queue = Queue::new(8).unwrap();
        writer.write_all(b"abcde").unwrap();
        let mut data = [0u8; 2];
        assert_eq!(queue.read(reader.as_fd(), &mut data, WAIT).unwrap(), 2);
        assert_eq!(&data, b"ab");
        let mut rest = [0u8; 8];
        assert_eq!(queue.read(reader.as_fd(), &mut rest, WAIT).unwrap(), 3);
        assert_eq!(&rest[..3], b"cde");
        writer.write_all(b"XY").unwrap();
        assert_eq!(queue.read(reader.as_fd(), &mut data, WAIT).unwrap(), 2);
        assert_eq!(&data, b"XY");
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
    fn cancelling_an_idle_queue_leaves_what_it_did_not_take_in_the_file() {
        // A read takes only what its caller asked for, so nothing is left in the queue to forget:
        // the bytes not taken are still in the pipe after a cancel.
        let (reader, mut writer) = std::io::pipe().unwrap();
        let mut queue = Queue::new(8).unwrap();
        writer.write_all(b"old").unwrap();
        assert_eq!(queue.read(reader.as_fd(), &mut [0u8; 1], WAIT).unwrap(), 1);
        queue.cancel();
        writer.write_all(b"new").unwrap();
        let mut data = [0u8; 8];
        assert_eq!(queue.read(reader.as_fd(), &mut data, WAIT).unwrap(), 5);
        assert_eq!(&data[..5], b"ldnew");
    }

    // No file a test can open here completes later than `io_submit` returns, so a transfer
    // still under way when its call returns, and cancelling one, are for a board run.

    #[test]
    fn a_ring_writes_each_transfer_from_its_own_memory_in_order() {
        let (mut reader, writer) = std::io::pipe().unwrap();
        let mut ring = Ring::new(3, 4).unwrap();
        assert_eq!((ring.transfers(), ring.capacity()), (3, 4));
        for chunk in [&b"ab"[..], b"cdef", b"g"] {
            assert_eq!(
                ring.write(writer.as_fd(), chunk, WAIT).unwrap(),
                chunk.len()
            );
        }
        // A transfer's worth at most, and nothing is taken of more.
        let error = ring.write(writer.as_fd(), b"hijkl", WAIT).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        // Every transfer comes back free, so a ring of three takes a fourth and more.
        for _ in 0..6 {
            ring.write(writer.as_fd(), b"z", WAIT).unwrap();
        }
        drop(writer);
        let mut all = Vec::new();
        std::io::Read::read_to_end(&mut reader, &mut all).unwrap();
        assert_eq!(all, b"abcdefgzzzzzz");
        // A ring goes one way.
        let (other, _) = std::io::pipe().unwrap();
        let error = ring.read(other.as_fd(), &mut [0u8; 4], WAIT).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn a_write_that_failed_is_not_reported_and_the_ring_goes_on() {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let mut ring = Ring::new(2, 4).unwrap();
        // Twice the transfers there are: each failed write freed its transfer for the next.
        for _ in 0..4 {
            assert_eq!(ring.write(writer.as_fd(), b"x", WAIT).unwrap(), 1);
        }
    }

    #[test]
    fn a_ring_keeps_every_transfer_reading_and_hands_out_the_oldest_first() {
        let (reader, mut writer) = std::io::pipe().unwrap();
        // An empty pipe that doesn't block answers "try again" in place.
        nonblocking(reader.as_fd());
        writer.write_all(b"abcdef").unwrap();
        let mut ring = Ring::new(3, 2).unwrap();
        let mut data = [0u8; 2];
        // The first call hands all three to the kernel: each takes its two bytes in turn.
        for expected in [b"ab", b"cd", b"ef"] {
            assert_eq!(ring.read(reader.as_fd(), &mut data, WAIT).unwrap(), 2);
            assert_eq!(&data, expected);
        }
        // Each went back to the kernel as it was taken, and found the pipe empty.
        for _ in 0..3 {
            let error = ring.read(reader.as_fd(), &mut data, WAIT).unwrap_err();
            assert_eq!(error.raw_os_error(), Some(libc::EAGAIN));
        }
        writer.write_all(b"gh").unwrap();
        // All three went back before the bytes came, so they come in the fourth read, which
        // the first of them began as it was handed out.
        for _ in 0..3 {
            let error = ring.read(reader.as_fd(), &mut data, WAIT).unwrap_err();
            assert_eq!(error.raw_os_error(), Some(libc::EAGAIN));
        }
        assert_eq!(ring.read(reader.as_fd(), &mut data, WAIT).unwrap(), 2);
        assert_eq!(&data, b"gh");
        // A ring goes one way.
        let error = ring.write(writer.as_fd(), b"x", WAIT).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn a_short_room_takes_what_fits_and_cancelling_forgets_what_was_read() {
        let (reader, mut writer) = std::io::pipe().unwrap();
        nonblocking(reader.as_fd());
        writer.write_all(b"oldest").unwrap();
        let mut ring = Ring::new(2, 8).unwrap();
        let mut small = [0u8; 3];
        assert_eq!(ring.read(reader.as_fd(), &mut small, WAIT).unwrap(), 3);
        assert_eq!(&small, b"old");
        writer.write_all(b"new").unwrap();
        ring.cancel();
        assert_eq!(ring.under_way(), 0);
        let mut data = [0u8; 8];
        assert_eq!(ring.read(reader.as_fd(), &mut data, WAIT).unwrap(), 3);
        assert_eq!(&data[..3], b"new");
        // Cancelled, it may go the other way.
        ring.cancel();
        ring.write(writer.as_fd(), b"w", WAIT).unwrap();
    }

    #[test]
    fn a_ring_of_no_transfers_or_too_many_is_refused() {
        for transfers in [0, MAX_RING + 1] {
            let error = Ring::new(transfers, 4).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn a_ring_allocates_nothing_once_made() {
        let (reader, writer) = std::io::pipe().unwrap();
        nonblocking(reader.as_fd());
        let mut writes = Ring::new(4, 8).unwrap();
        let mut reads = Ring::new(4, 8).unwrap();
        let mut data = [0u8; 8];
        writes.write(writer.as_fd(), b"warm", WAIT).unwrap();
        let _ = reads.read(reader.as_fd(), &mut data, WAIT);
        let ((), allocated) = crate::test_alloc::allocations(|| {
            for _ in 0..100 {
                writes.write(writer.as_fd(), b"abcd", WAIT).unwrap();
                let _ = reads.read(reader.as_fd(), &mut data, WAIT);
            }
        });
        assert_eq!(allocated, 0);
        drop(writer);
    }
}
