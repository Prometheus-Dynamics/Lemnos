//! FunctionFS (`usb_f_fs`): a USB function served from user space through the files of a
//! mounted `functionfs` instance. `ep0` takes the function's descriptors and strings, then
//! reports events and carries control requests; one file per endpoint carries its data.
//!
//! Provenance: the Linux uAPI headers are GPL-2.0 WITH Linux-syscall-note. The magic numbers,
//! flags, blob formats, the event layout and numbering and the ioctl numbers below are
//! interface facts transcribed from `<linux/usb/functionfs.h>` (the installed header of kernel
//! 7.2, the same since `FUNCTIONFS_ALL_CTRL_RECIP` arrived in 4.9). What reading and writing
//! the files does (the wrong-way transfer that stalls and the error codes it answers with) is
//! from `drivers/usb/gadget/function/f_fs.c` and `Documentation/usb/functionfs.rst`. No kernel
//! code is copied.
//!
//! Nothing here is `unsafe` but the one ioctl; the rest is the layouts and what each read and
//! write means. Everything read from the kernel is checked before use.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::ioctl::{ioc, ioctl_int};

/// `FUNCTIONFS_STRINGS_MAGIC`.
pub const STRINGS_MAGIC: u32 = 2;
/// `FUNCTIONFS_DESCRIPTORS_MAGIC_V2`.
pub const DESCRIPTORS_MAGIC_V2: u32 = 3;

/// `FUNCTIONFS_HAS_FS_DESC`: the blob has full-speed descriptors.
pub const HAS_FS_DESC: u32 = 1;
/// `FUNCTIONFS_HAS_HS_DESC`: the blob has high-speed descriptors.
pub const HAS_HS_DESC: u32 = 2;
/// `FUNCTIONFS_HAS_SS_DESC`.
pub const HAS_SS_DESC: u32 = 4;
/// `FUNCTIONFS_HAS_MS_OS_DESC`.
pub const HAS_MS_OS_DESC: u32 = 8;
/// `FUNCTIONFS_VIRTUAL_ADDR`: endpoint files are named by the addresses in the descriptors
/// (`ep81`), and a request to an endpoint carries that address, whatever address the controller
/// gave the endpoint on the wire.
pub const VIRTUAL_ADDR: u32 = 16;
/// `FUNCTIONFS_EVENTFD`.
pub const EVENTFD: u32 = 32;
/// `FUNCTIONFS_ALL_CTRL_RECIP`: class and vendor requests to any recipient reach the function,
/// not only those to its interfaces and endpoints.
pub const ALL_CTRL_RECIP: u32 = 64;
/// `FUNCTIONFS_CONFIG0_SETUP`.
pub const CONFIG0_SETUP: u32 = 128;

/// `FUNCTIONFS_FIFO_STATUS`: how many bytes sit unclaimed in the endpoint's FIFO.
pub const FIFO_STATUS: u32 = ioc(0, b'g', 1, 0);
/// `FUNCTIONFS_FIFO_FLUSH`: discards them.
pub const FIFO_FLUSH: u32 = ioc(0, b'g', 2, 0);
/// `FUNCTIONFS_CLEAR_HALT`: clears the endpoint's halt and resets its data toggle.
pub const CLEAR_HALT: u32 = ioc(0, b'g', 3, 0);

/// Bytes in a `struct usb_functionfs_event`: the 8 of a setup packet, the type, 3 of padding.
pub const EVENT_LEN: usize = 12;

/// What a transfer in the wrong direction on `ep0` answers once it has stalled the pending
/// request (`EL2HLT`).
pub const STALLED: i32 = libc::EL2HLT;
/// What a transfer in the wrong direction on an endpoint file answers once it has halted the
/// endpoint (`EBADMSG`).
pub const HALTED: i32 = libc::EBADMSG;
/// What `ep0` answers when the host abandoned the request being answered (`EIDRM`).
pub const ABANDONED: i32 = libc::EIDRM;
/// What `ep0` answers when there is no request to stall (`ESRCH`).
pub const NO_REQUEST: i32 = libc::ESRCH;

/// A setup packet, in the host's byte order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setup {
    /// `bRequestType`.
    pub request_type: u8,
    /// `bRequest`.
    pub request: u8,
    /// `wValue`.
    pub value: u16,
    /// `wIndex`.
    pub index: u16,
    /// `wLength`.
    pub length: u16,
}

impl Setup {
    /// Whether the data stage goes to the host (`USB_DIR_IN`).
    pub const fn is_in(&self) -> bool {
        self.request_type & 0x80 != 0
    }
}

/// `enum usb_functionfs_event_type`, with the setup packet where there is one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The gadget was bound to a controller.
    Bind,
    /// The gadget was unbound from its controller.
    Unbind,
    /// The host chose the configuration: endpoints work.
    Enable,
    /// The configuration is gone: by the host's choice, a reset or a disconnect.
    Disable,
    /// A control request, pending until its data stage is run or it is stalled.
    Setup(Setup),
    /// The bus was suspended.
    Suspend,
    /// The bus was resumed.
    Resume,
}

impl Event {
    /// Reads a `struct usb_functionfs_event`. `None` for a type this does not know.
    pub fn parse(bytes: &[u8; EVENT_LEN]) -> Option<Self> {
        Some(match bytes[8] {
            0 => Self::Bind,
            1 => Self::Unbind,
            2 => Self::Enable,
            3 => Self::Disable,
            4 => Self::Setup(Setup {
                request_type: bytes[0],
                request: bytes[1],
                value: u16::from_le_bytes([bytes[2], bytes[3]]),
                index: u16::from_le_bytes([bytes[4], bytes[5]]),
                length: u16::from_le_bytes([bytes[6], bytes[7]]),
            }),
            5 => Self::Suspend,
            6 => Self::Resume,
            _ => return None,
        })
    }
}

fn invalid() -> io::Error {
    io::ErrorKind::InvalidInput.into()
}

/// How many descriptors `bytes` holds, each led by its own length. `InvalidInput` when one
/// doesn't fit.
fn count(bytes: &[u8]) -> io::Result<u32> {
    let mut rest = bytes;
    let mut count = 0u32;
    while let Some(&len) = rest.first() {
        let len = usize::from(len);
        if len < 2 || len > rest.len() {
            return Err(invalid());
        }
        rest = &rest[len..];
        count += 1;
    }
    Ok(count)
}

/// The descriptors blob `ep0` takes first (format 2): the magic, the blob's length, `flags`
/// with the speed bits added, a count per speed present, then the descriptors of each speed.
///
/// `full` and `high` are the interface, endpoint and class descriptors of the function at that
/// speed, back to back, without a configuration descriptor. At least one of them must be
/// given. `flags` are other `FUNCTIONFS_*` flags that add no field to the blob.
pub fn descriptors_blob(
    flags: u32,
    full: Option<&[u8]>,
    high: Option<&[u8]>,
) -> io::Result<Vec<u8>> {
    if flags & (HAS_FS_DESC | HAS_HS_DESC | HAS_SS_DESC | HAS_MS_OS_DESC | EVENTFD) != 0
        || (full.is_none() && high.is_none())
    {
        return Err(invalid());
    }
    let mut flags = flags;
    let mut blob = vec![0u8; 12];
    for (speed, bit) in [(full, HAS_FS_DESC), (high, HAS_HS_DESC)] {
        if let Some(descriptors) = speed {
            flags |= bit;
            blob.extend_from_slice(&count(descriptors)?.to_le_bytes());
        }
    }
    for descriptors in [full, high].into_iter().flatten() {
        blob.extend_from_slice(descriptors);
    }
    let length = u32::try_from(blob.len()).map_err(|_| invalid())?;
    blob[..4].copy_from_slice(&DESCRIPTORS_MAGIC_V2.to_le_bytes());
    blob[4..8].copy_from_slice(&length.to_le_bytes());
    blob[8..12].copy_from_slice(&flags.to_le_bytes());
    Ok(blob)
}

/// The strings blob `ep0` takes second: the magic, the blob's length, the number of strings,
/// the number of languages, then for the one language its code and each string in UTF-8 with a
/// NUL after it. The function's descriptors name these strings by their place here, from 1.
///
/// With no strings the blob says no language either, as the kernel requires. A string with a
/// NUL in it is `InvalidInput`.
pub fn strings_blob(language: u16, strings: &[&str]) -> io::Result<Vec<u8>> {
    let count = u32::try_from(strings.len()).map_err(|_| invalid())?;
    let mut blob = vec![0u8; 16];
    if count != 0 {
        blob.extend_from_slice(&language.to_le_bytes());
    }
    for string in strings {
        if string.contains('\0') {
            return Err(invalid());
        }
        blob.extend_from_slice(string.as_bytes());
        blob.push(0);
    }
    let length = u32::try_from(blob.len()).map_err(|_| invalid())?;
    blob[..4].copy_from_slice(&STRINGS_MAGIC.to_le_bytes());
    blob[4..8].copy_from_slice(&length.to_le_bytes());
    blob[8..12].copy_from_slice(&count.to_le_bytes());
    blob[12..16].copy_from_slice(&u32::from(count != 0).to_le_bytes());
    Ok(blob)
}

fn open(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

/// One transfer the wrong way, which FunctionFS takes as "stall this". `Ok` when it answered
/// with `expected`, which is how it says it has.
fn wrong_way(mut file: &File, read: bool, expected: i32) -> io::Result<()> {
    let mut byte = [0u8; 1];
    let done = if read {
        file.read(&mut byte)
    } else {
        file.write(&byte)
    };
    match done {
        Err(error) if error.raw_os_error() == Some(expected) => Ok(()),
        Err(error) => Err(error),
        // Not FunctionFS: a file that carried the byte instead of stalling.
        Ok(_) => Err(io::ErrorKind::InvalidData.into()),
    }
}

/// The `ep0` file of a mounted FunctionFS instance, opened for reading and writing without
/// blocking on events. Closing it takes the function, and with it the gadget, off its
/// controller.
#[derive(Debug)]
pub struct Ep0 {
    file: File,
}

impl Ep0 {
    /// Opens `path`, the `ep0` of a mounted instance. It is never created.
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self { file: open(path)? })
    }

    /// Gives the function its descriptors (a [`descriptors_blob`]) and then its strings (a
    /// [`strings_blob`]), each in the one write the kernel requires. The endpoint files exist
    /// once both are in.
    pub fn describe(&self, descriptors: &[u8], strings: &[u8]) -> io::Result<()> {
        for blob in [descriptors, strings] {
            if (&self.file).write(blob)? != blob.len() {
                return Err(io::ErrorKind::WriteZero.into());
            }
        }
        Ok(())
    }

    /// The next event, or `None` when there is none. Only to be called when no control request
    /// is pending: with one pending, a read is its data stage or its stall.
    pub fn next_event(&self) -> io::Result<Option<Event>> {
        let mut bytes = [0u8; EVENT_LEN];
        match (&self.file).read(&mut bytes) {
            Ok(0) => Ok(None),
            Ok(EVENT_LEN) => Event::parse(&bytes)
                .map(Some)
                .ok_or_else(|| io::ErrorKind::InvalidData.into()),
            Ok(_) => Err(io::ErrorKind::InvalidData.into()),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Sends `data` as the data stage of the pending request, which must be one whose data
    /// goes to the host, and returns how much went. Waits for the host to take it.
    pub fn send(&self, data: &[u8]) -> io::Result<usize> {
        (&self.file).write(data)
    }

    /// Takes the data stage of the pending request, which must be one whose data comes from
    /// the host, and so accepts it. An empty `data` accepts a request that has no data stage.
    /// Waits for the host to send it.
    pub fn receive(&self, data: &mut [u8]) -> io::Result<usize> {
        let got = (&self.file).read(data)?;
        if got > data.len() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(got)
    }

    /// Stalls the pending request. `request_is_in` is whether its data goes to the host: the
    /// stall is asked for by a transfer the other way. An error with [`ABANDONED`] or
    /// [`NO_REQUEST`] when the host had already let the request go.
    pub fn stall(&self, request_is_in: bool) -> io::Result<()> {
        wrong_way(&self.file, request_is_in, STALLED)
    }
}

/// One endpoint file of a mounted FunctionFS instance, opened for reading and writing so that
/// it can be halted. Transfers on it block, and so go through [`crate::aio`]; opened
/// non-blocking, a transfer on an endpoint the host has not enabled fails with `EAGAIN`
/// instead of waiting for the host.
#[derive(Debug)]
pub struct Endpoint {
    file: File,
}

impl Endpoint {
    /// Opens `path`, an endpoint file. It is never created.
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self { file: open(path)? })
    }

    /// Halts the endpoint. `endpoint_is_in` is whether it sends to the host: the halt is
    /// asked for by a transfer the other way. Isochronous endpoints answer `EINVAL`.
    pub fn halt(&self, endpoint_is_in: bool) -> io::Result<()> {
        wrong_way(&self.file, endpoint_is_in, HALTED)
    }

    /// Clears the endpoint's halt and resets its data toggle (`FUNCTIONFS_CLEAR_HALT`).
    pub fn clear_halt(&self) -> io::Result<()> {
        ioctl_int(self.file.as_fd(), CLEAR_HALT, 0).map(drop)
    }
}

impl AsFd for Endpoint {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

#[cfg(test)]
mod tests;
