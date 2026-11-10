//! The open `vvcam-isp` device: register reads and writes bounded by [`crate::access`], and the
//! ISP's interrupt as events.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::path::Path;

use crate::access::{allowed, readable, writable};
use crate::regs::{ISP_IMSC, MIS_FRAME_OUT};
use crate::uapi::{
    DQEVENT, EID_ISP_MIS, EVENT_IRQ_TYPE, Event, READ_REG, RegPair, SUBSCRIBE_EVENT, Subscription,
    WRITE_REG,
};

/// The structures the requests here carry. Sealed: only this crate's `#[repr(C)]` integer
/// structures, none of which holds a pointer.
trait Arg: Copy {}
impl Arg for RegPair {}
impl Arg for Subscription {}
impl Arg for Event {}

/// Issues `request` with `arg`, retrying on `EINTR`. Private: each caller below pairs a request
/// with the structure its number encodes.
fn call<T: Arg>(fd: BorrowedFd<'_>, request: u32, arg: &mut T) -> io::Result<()> {
    let ptr = std::ptr::from_mut(arg);
    loop {
        // SAFETY: `request` is one of this crate's numbers, made with `size_of::<T>()` for the
        // `T` it is called with (the callers below, checked by the layout tests); `ptr` points to
        // a live, exclusive `T` for the whole call; `T` holds integers only, so the kernel
        // follows no pointer in it; `fd` is open for the duration of the call.
        let ret = unsafe { libc::ioctl(fd.as_raw_fd(), request as _, ptr) };
        if ret >= 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// An open `vvcam-isp` device (normally `/dev/vvcam-isp.0`; the path is always the caller's).
#[derive(Debug)]
pub struct IspNode {
    file: File,
}

impl IspNode {
    /// Opens the device at `path` for reading and writing.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self { file })
    }

    fn fd(&self) -> BorrowedFd<'_> {
        use std::os::fd::AsFd;
        self.file.as_fd()
    }

    fn raw_read(&self, addr: u32) -> io::Result<u32> {
        let mut pair = RegPair { addr, value: 0 };
        call(self.fd(), READ_REG, &mut pair)?;
        Ok(pair.value)
    }

    fn raw_write(&self, addr: u32, value: u32) -> io::Result<()> {
        let mut pair = RegPair { addr, value };
        call(self.fd(), WRITE_REG, &mut pair)
    }

    /// The register at `addr`. `InvalidInput` for a register [`readable`] refuses, without
    /// reaching the device.
    pub fn read(&self, addr: u32) -> io::Result<u32> {
        if !readable(addr) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        self.raw_read(addr)
    }

    /// Writes `value` to the register at `addr`. `InvalidInput`, without reaching the device, for
    /// a register [`writable`] refuses or a value with a bit outside the register's mask
    /// ([`allowed`]). A register whose mask is not every bit is read first and its other bits
    /// written back as they were.
    pub fn write(&self, addr: u32, value: u32) -> io::Result<()> {
        let Some(mask) = writable(addr) else {
            return Err(io::ErrorKind::InvalidInput.into());
        };
        if !allowed(addr, value) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let value = if mask == u32::MAX {
            value
        } else {
            (self.raw_read(addr)? & !mask) | value
        };
        self.raw_write(addr, value)
    }

    /// Queues the ISP core's interrupt on this file as events ([`Self::next_event`]) and lets a
    /// frame leaving the ISP raise it: sets [`MIS_FRAME_OUT`] in [`ISP_IMSC`], keeping the
    /// mask's other bits.
    pub fn watch_frames(&self) -> io::Result<()> {
        let mut sub = Subscription {
            kind: EVENT_IRQ_TYPE,
            id: EID_ISP_MIS,
        };
        call(self.fd(), SUBSCRIBE_EVENT, &mut sub)?;
        let mask = self.raw_read(ISP_IMSC)?;
        if mask & MIS_FRAME_OUT == 0 {
            self.raw_write(ISP_IMSC, mask | MIS_FRAME_OUT)?;
        }
        Ok(())
    }

    /// The next queued event, or `None` when none is queued. Never blocks.
    pub fn next_event(&self) -> io::Result<Option<Event>> {
        let mut event = Event::default();
        match call(self.fd(), DQEVENT, &mut event) {
            Ok(()) => Ok(Some(event)),
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
