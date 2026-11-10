//! The two device nodes, each request issued as it stands. Nothing here checks a register: the
//! ISP node's raw requests are private to the crate and reached only through
//! [`Capture`](crate::Capture), which does.

// The tests stand a fake in for the ISP node, so only its open path and the MIPI node are used
// there.
#![cfg_attr(test, allow(dead_code))]

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::path::Path;

use lemnos_driver_k230isp_sys::uapi::{
    DQEVENT, EVENT_IRQ_TYPE, Event, READ_REG, RegPair, SUBSCRIBE_EVENT, Subscription, WRITE_REG,
};

use crate::mipi::{DevAttr, SET_DEV_ATTR};

/// The structures the requests here carry. Sealed: only `#[repr(C)]` integer structures, none
/// of which holds a pointer.
trait Arg: Copy {}
impl Arg for RegPair {}
impl Arg for Subscription {}
impl Arg for Event {}
impl Arg for DevAttr {}

/// Issues `request` with `arg`, retrying on `EINTR`. Private: each caller below pairs a request
/// with the structure its number encodes.
fn call<T: Arg>(fd: BorrowedFd<'_>, request: u32, arg: &mut T) -> io::Result<()> {
    let ptr = std::ptr::from_mut(arg);
    loop {
        // SAFETY: `request` is one of the numbers below, each made with `size_of::<T>()` for the
        // `T` it is called with (checked by the layout tests and `layout-check`); `ptr` points to
        // a live, exclusive `T` for the whole call; `T` holds integers only, so the kernel follows
        // no pointer in it; `fd` is open for the duration of the call.
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

fn open(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open(path)
}

/// An open `vvcam-isp` device, unchecked. Crate-private.
#[derive(Debug)]
pub(crate) struct IspNode {
    file: File,
}

impl IspNode {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        Ok(Self { file: open(path)? })
    }

    fn fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }

    pub(crate) fn read_raw(&self, addr: u32) -> io::Result<u32> {
        let mut pair = RegPair { addr, value: 0 };
        call(self.fd(), READ_REG, &mut pair)?;
        Ok(pair.value)
    }

    pub(crate) fn write_raw(&self, addr: u32, value: u32) -> io::Result<()> {
        let mut pair = RegPair { addr, value };
        call(self.fd(), WRITE_REG, &mut pair)
    }

    pub(crate) fn subscribe(&self, id: u32) -> io::Result<()> {
        let mut sub = Subscription {
            kind: EVENT_IRQ_TYPE,
            id,
        };
        call(self.fd(), SUBSCRIBE_EVENT, &mut sub)
    }

    /// The next queued event, or `None`. Never blocks (`DQEVENT` answers `ENOENT` when empty).
    pub(crate) fn dequeue(&self) -> io::Result<Option<Event>> {
        let mut event = Event::default();
        match call(self.fd(), DQEVENT, &mut event) {
            Ok(()) => Ok(Some(event)),
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// An open `vvcam-mipi` device (normally `/dev/vvcam-mipi.0`; the path is always the caller's).
/// Its one request carries plain numbers; see [`crate::mipi`].
#[derive(Debug)]
pub struct MipiNode {
    file: File,
}

impl MipiNode {
    /// Opens the device at `path` for reading and writing.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            file: open(path.as_ref())?,
        })
    }

    /// Hands the kernel the link (`VVCAM_MIPI_SET_DEV_ATTR`), which then sets up the CSI
    /// receiver, its D-PHY and the video input feeding the ISP.
    pub fn set_link(&self, attr: &DevAttr) -> io::Result<()> {
        let mut attr = *attr;
        call(self.file.as_fd(), SET_DEV_ATTR, &mut attr)
    }
}
