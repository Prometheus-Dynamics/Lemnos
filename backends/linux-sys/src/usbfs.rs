//! usbdevfs (`/dev/bus/usb/BBB/DDD`): control and bulk transfers, interface claims, alternate
//! settings, detaching a kernel driver, the port reset, the descriptor read and a ring of
//! isochronous URBs. Ported from `<linux/usbdevice_fs.h>`; what the kernel answers to the
//! detach and reset requests is from `drivers/usb/core/devio.c` and `hub.c`.
//!
//! Every pointer the kernel is given comes from a slice or from memory this module owns, and its
//! length from that slice or allocation: there is no call that takes an address, or a length apart
//! from the buffer it describes. The direction of a transfer is checked too. The kernel writes
//! through the pointer whenever the request or endpoint says IN, so an OUT call that named an IN
//! endpoint would have it write through a `&[u8]`.
//!
//! What the kernel reports back is checked before it is used: a count larger than the buffer it
//! refers to is an error. Malformed reports are [`io::ErrorKind::InvalidData`].

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsFd;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::time::Duration;

use crate::ioctl::{ioctl_int, ioctl_ptr};
use crate::poll::{POLLERR, POLLHUP, POLLNVAL, POLLOUT, poll_one};

#[cfg(any(test, feature = "test-usbfs"))]
pub mod fake;
mod ring;

pub use ring::{MAX_ISO_PACKETS, RingCompletion, RingConfig, RingPacket, UrbRing};

const USB: u8 = b'U';

const CONTROL: u32 = crate::iowr::<CtrlTransfer>(USB, 0);
const BULK: u32 = crate::iowr::<BulkTransfer>(USB, 2);
const SETINTERFACE: u32 = crate::ior::<SetInterface>(USB, 4);
const GETDRIVER: u32 = crate::iow::<GetDriver>(USB, 8);
const SUBMITURB: u32 = crate::ior::<UrbHeader>(USB, 10);
const DISCARDURB: u32 = crate::ioc(0, USB, 11, 0);
const REAPURBNDELAY: u32 = crate::iow::<*mut c_void>(USB, 13);
const CLAIMINTERFACE: u32 = crate::ior::<libc::c_uint>(USB, 15);
const RELEASEINTERFACE: u32 = crate::ior::<libc::c_uint>(USB, 16);
const IOCTL: u32 = crate::iowr::<InterfaceIoctl>(USB, 18);
const RESET: u32 = crate::ioc(0, USB, 20, 0);
/// Not issued on the file itself: it is the `ioctl_code` of a [`IOCTL`] on one interface.
const DISCONNECT: u32 = crate::ioc(0, USB, 22, 0);

/// `USBDEVFS_MAXDRIVERNAME + 1`.
const DRIVER_NAME_BYTES: usize = 256;
/// The driver name the kernel reports for an interface claimed through usbdevfs, by this file
/// or by any other process.
const USBFS_DRIVER: &[u8] = b"usbfs";

/// `USBDEVFS_URB_TYPE_ISO`.
const URB_TYPE_ISO: u8 = 0;
/// `USBDEVFS_URB_ISO_ASAP`: start isochronous URBs at the next free frame.
const URB_ISO_ASAP: u32 = 0x2;

/// The IN bit of a `bEndpointAddress` and of a `bmRequestType`.
const DIR_IN: u8 = 0x80;
/// The bits of a `bEndpointAddress` that are always zero.
const ENDPOINT_RESERVED: u8 = 0x70;
/// The endpoint number within a `bEndpointAddress`.
const ENDPOINT_NUMBER: u8 = 0x0f;

/// `struct usbdevfs_ctrltransfer`.
#[repr(C)]
#[derive(Clone, Copy)]
struct CtrlTransfer {
    request_type: u8,
    request: u8,
    value: u16,
    index: u16,
    length: u16,
    timeout: u32,
    data: *mut c_void,
}

/// `struct usbdevfs_bulktransfer`.
#[repr(C)]
#[derive(Clone, Copy)]
struct BulkTransfer {
    endpoint: u32,
    length: u32,
    timeout: u32,
    data: *mut c_void,
}

/// `struct usbdevfs_setinterface`.
#[repr(C)]
#[derive(Clone, Copy)]
struct SetInterface {
    interface: u32,
    alternate: u32,
}

/// `struct usbdevfs_getdriver`.
#[repr(C)]
#[derive(Clone, Copy)]
struct GetDriver {
    interface: u32,
    driver: [u8; DRIVER_NAME_BYTES],
}

/// `struct usbdevfs_ioctl`: a request passed on to one interface.
#[repr(C)]
#[derive(Clone, Copy)]
struct InterfaceIoctl {
    ifno: i32,
    ioctl_code: i32,
    data: *mut c_void,
}

/// `struct usbdevfs_urb`: the header the kernel reads on submit and writes on completion. Its
/// isochronous descriptors follow it in memory.
#[repr(C)]
#[derive(Clone, Copy)]
struct UrbHeader {
    kind: u8,
    endpoint: u8,
    status: i32,
    flags: u32,
    buffer: *mut c_void,
    buffer_length: i32,
    actual_length: i32,
    start_frame: i32,
    number_of_packets: i32,
    error_count: i32,
    signr: u32,
    usercontext: *mut c_void,
}

/// `struct usbdevfs_iso_packet_desc`.
#[repr(C)]
#[derive(Clone, Copy)]
struct IsoPacketDesc {
    length: u32,
    actual_length: u32,
    status: u32,
}

/// A control request's setup fields. The data length is the length of the slice passed with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlSetup {
    /// `bmRequestType`: direction (bit 7), type (bits 5-6), recipient (bits 0-4).
    pub request_type: u8,
    /// `bRequest`.
    pub request: u8,
    /// `wValue`.
    pub value: u16,
    /// `wIndex`.
    pub index: u16,
}

/// What a wait on the device file found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// The timeout passed.
    Idle,
    /// A completed URB is waiting to be reaped.
    Completion,
    /// The device left, and nothing further will complete.
    Gone,
}

/// The kernel's side of the calls: the device file, or a recording stand-in for tests.
pub(crate) enum Channel {
    Device(File),
    #[cfg(any(test, feature = "test-usbfs"))]
    Fake(fake::FakeUsbfs),
}

impl Channel {
    /// Issues `request` with a pointer argument.
    ///
    /// # Safety
    ///
    /// As for [`ioctl_ptr`]: `arg` must be a live value of the type `request` encodes, valid for
    /// the reads and writes the request performs, including through pointers it contains.
    unsafe fn ioctl_ptr<T>(&self, request: u32, arg: *mut T) -> io::Result<libc::c_int> {
        match self {
            // SAFETY: the caller holds up this function's contract, which is `ioctl_ptr`'s.
            Self::Device(file) => unsafe { ioctl_ptr(file.as_fd(), request, arg) },
            #[cfg(any(test, feature = "test-usbfs"))]
            // SAFETY: the caller holds up this function's contract, which is the fake's.
            Self::Fake(fake) => unsafe { fake.ioctl_ptr(request, arg.cast()) },
        }
    }

    /// Issues `request` with an argument passed by value.
    fn ioctl_int(&self, request: u32, arg: libc::c_ulong) -> io::Result<libc::c_int> {
        match self {
            Self::Device(file) => ioctl_int(file.as_fd(), request, arg),
            #[cfg(any(test, feature = "test-usbfs"))]
            Self::Fake(fake) => fake.ioctl_int(request, arg),
        }
    }

    fn read_at(&self, out: &mut [u8], offset: u64) -> io::Result<usize> {
        match self {
            Self::Device(file) => loop {
                match file.read_at(out, offset) {
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    other => return other,
                }
            },
            #[cfg(any(test, feature = "test-usbfs"))]
            Self::Fake(fake) => fake.read_at(out, offset),
        }
    }

    fn wait(&self, timeout: Duration) -> io::Result<Readiness> {
        let revents = match self {
            Self::Device(file) => poll_one(file.as_fd(), POLLOUT, Some(timeout))?,
            #[cfg(any(test, feature = "test-usbfs"))]
            Self::Fake(fake) => fake.poll(timeout),
        };
        // A completion is taken before a hang-up: the kernel still hands those back.
        Ok(if revents & POLLOUT != 0 {
            Readiness::Completion
        } else if revents & (POLLERR | POLLHUP | POLLNVAL) != 0 {
            Readiness::Gone
        } else {
            Readiness::Idle
        })
    }

    /// A second handle on the same open file: the kernel keeps URBs per open file.
    fn duplicate(&self) -> io::Result<Self> {
        match self {
            Self::Device(file) => file.try_clone().map(Self::Device),
            #[cfg(any(test, feature = "test-usbfs"))]
            Self::Fake(fake) => Ok(Self::Fake(fake.clone())),
        }
    }
}

/// An open usbdevfs device file.
pub struct UsbfsFile {
    channel: Channel,
}

impl std::fmt::Debug for UsbfsFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UsbfsFile")
    }
}

impl UsbfsFile {
    /// Opens a device node read-write.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self {
            channel: Channel::Device(file),
        })
    }

    /// A file served by `fake`, for tests.
    #[cfg(any(test, feature = "test-usbfs"))]
    pub fn fake(fake: fake::FakeUsbfs) -> Self {
        Self {
            channel: Channel::Fake(fake),
        }
    }

    /// Reads the device's raw descriptors at `offset`: the device descriptor, then each
    /// configuration. The read is positional, so it never moves the file offset.
    pub fn read_descriptors(&self, offset: u64, out: &mut [u8]) -> io::Result<usize> {
        self.channel.read_at(out, offset)
    }

    /// A control IN transfer into `data`; returns the bytes received. `request_type` must have
    /// the IN bit set.
    pub fn control_in(
        &self,
        setup: ControlSetup,
        data: &mut [u8],
        timeout_ms: u32,
    ) -> io::Result<usize> {
        if setup.request_type & DIR_IN == 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        self.control(setup, data.as_mut_ptr().cast(), data.len(), timeout_ms)
    }

    /// A control OUT transfer of `data`; returns the bytes sent. `request_type` must have the IN
    /// bit clear, since the kernel would write through `data` otherwise.
    pub fn control_out(
        &self,
        setup: ControlSetup,
        data: &[u8],
        timeout_ms: u32,
    ) -> io::Result<usize> {
        if setup.request_type & DIR_IN != 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        self.control(
            setup,
            data.as_ptr().cast_mut().cast(),
            data.len(),
            timeout_ms,
        )
    }

    /// `data` and `length` are one slice, and the caller checked its direction.
    fn control(
        &self,
        setup: ControlSetup,
        data: *mut c_void,
        length: usize,
        timeout_ms: u32,
    ) -> io::Result<usize> {
        let wlength = u16::try_from(length).map_err(|_| io::ErrorKind::InvalidInput)?;
        let mut arg = CtrlTransfer {
            request_type: setup.request_type,
            request: setup.request,
            value: setup.value,
            index: setup.index,
            length: wlength,
            timeout: timeout_ms,
            data,
        };
        // SAFETY: `arg` is a live `usbdevfs_ctrltransfer`; `data` points to `length` bytes that
        // outlive this synchronous call and are writable when the request says IN.
        let moved = unsafe { self.channel.ioctl_ptr(CONTROL, &raw mut arg) }?;
        checked_count(moved, length)
    }

    /// A bulk or interrupt IN transfer into `data` (`endpoint` with bit 7 set).
    pub fn bulk_in(&self, endpoint: u8, data: &mut [u8], timeout_ms: u32) -> io::Result<usize> {
        check_endpoint(endpoint, true)?;
        self.bulk(endpoint, data.as_mut_ptr().cast(), data.len(), timeout_ms)
    }

    /// A bulk or interrupt OUT transfer of `data` (`endpoint` with bit 7 clear).
    pub fn bulk_out(&self, endpoint: u8, data: &[u8], timeout_ms: u32) -> io::Result<usize> {
        check_endpoint(endpoint, false)?;
        self.bulk(
            endpoint,
            data.as_ptr().cast_mut().cast(),
            data.len(),
            timeout_ms,
        )
    }

    /// `data` and `length` are one slice, and the caller checked its direction.
    fn bulk(
        &self,
        endpoint: u8,
        data: *mut c_void,
        length: usize,
        timeout_ms: u32,
    ) -> io::Result<usize> {
        // The kernel refuses anything near `INT_MAX`.
        let wire = i32::try_from(length).map_err(|_| io::ErrorKind::InvalidInput)? as u32;
        let mut arg = BulkTransfer {
            endpoint: u32::from(endpoint),
            length: wire,
            timeout: timeout_ms,
            data,
        };
        // SAFETY: `arg` is a live `usbdevfs_bulktransfer`; `data` points to `length` bytes that
        // outlive this synchronous call and are writable when the endpoint says IN.
        let moved = unsafe { self.channel.ioctl_ptr(BULK, &raw mut arg) }?;
        checked_count(moved, length)
    }

    /// Claims interface `number`. A kernel driver bound to it is left alone, and the claim then
    /// fails with `EBUSY`.
    pub fn claim_interface(&self, number: u8) -> io::Result<()> {
        let mut n = libc::c_uint::from(number);
        // SAFETY: `n` is a live `unsigned int` the kernel reads.
        unsafe { self.channel.ioctl_ptr(CLAIMINTERFACE, &raw mut n) }?;
        Ok(())
    }

    /// Releases interface `number`.
    pub fn release_interface(&self, number: u8) -> io::Result<()> {
        let mut n = libc::c_uint::from(number);
        // SAFETY: `n` is a live `unsigned int` the kernel reads.
        unsafe { self.channel.ioctl_ptr(RELEASEINTERFACE, &raw mut n) }?;
        Ok(())
    }

    /// Selects alternate setting `alternate` of interface `number`.
    pub fn set_interface(&self, number: u8, alternate: u8) -> io::Result<()> {
        let mut arg = SetInterface {
            interface: u32::from(number),
            alternate: u32::from(alternate),
        };
        // SAFETY: `arg` is a live `usbdevfs_setinterface` the kernel reads.
        unsafe { self.channel.ioctl_ptr(SETINTERFACE, &raw mut arg) }?;
        Ok(())
    }

    /// Unbinds the kernel driver bound to interface `number`, and returns whether there was one.
    /// Nothing bound is `Ok(false)`, and so is an interface held through usbdevfs (the kernel
    /// names that driver `usbfs`): that is a claim, this file's or another process's, and
    /// disconnecting it would take the claim away. The kernel does not bind the driver again on
    /// its own when the interface is released or the file closed.
    ///
    /// The kernel answers the question for an interface the device doesn't have the same way as
    /// for one with nothing bound, so that is `Ok(false)` too.
    pub fn detach_kernel_driver(&self, number: u8) -> io::Result<bool> {
        let mut query = GetDriver {
            interface: u32::from(number),
            driver: [0; DRIVER_NAME_BYTES],
        };
        // SAFETY: `query` is a live `usbdevfs_getdriver` the kernel reads and fills.
        let asked = unsafe { self.channel.ioctl_ptr(GETDRIVER, &raw mut query) };
        if nothing_bound(asked)? || driver_name(&query.driver) == USBFS_DRIVER {
            return Ok(false);
        }
        let mut arg = InterfaceIoctl {
            ifno: i32::from(number),
            ioctl_code: DISCONNECT as i32,
            data: std::ptr::null_mut(),
        };
        // SAFETY: `arg` is a live `usbdevfs_ioctl`. Its `ioctl_code` encodes no data, so the
        // kernel never reads through the null `data`.
        let detached = unsafe { self.channel.ioctl_ptr(IOCTL, &raw mut arg) };
        // The driver can let go by itself between the question and the request.
        Ok(!nothing_bound(detached)?)
    }

    /// Resets the device at its port. `ENODEV` when the device is no longer the one this file
    /// opened: it came back with other descriptors, so the kernel disconnected it and
    /// enumerates it as a new device, or it had already left. Every later call on the file then
    /// fails with `ENODEV` as well. When the device comes back unchanged, the file's claims
    /// stand.
    pub fn reset(&self) -> io::Result<()> {
        self.channel.ioctl_int(RESET, 0)?;
        Ok(())
    }

    /// A ring of isochronous URBs on this file. Nothing else may submit or reap on the file while
    /// the ring exists (see [`UrbRing`]). Control and bulk calls carry on: they don't reap.
    pub fn urb_ring(&self, config: RingConfig) -> io::Result<UrbRing> {
        UrbRing::new(self.channel.duplicate()?, config)
    }
}

/// Whether the kernel said no driver is bound (`ENODATA`). Any other failure is the caller's.
fn nothing_bound(result: io::Result<libc::c_int>) -> io::Result<bool> {
    match result {
        Ok(_) => Ok(false),
        Err(error) if error.raw_os_error() == Some(libc::ENODATA) => Ok(true),
        Err(error) => Err(error),
    }
}

/// The driver name up to its terminator, or all of it when the kernel left none.
fn driver_name(driver: &[u8; DRIVER_NAME_BYTES]) -> &[u8] {
    let end = driver
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(driver.len());
    &driver[..end]
}

/// The count an ioctl returned, as long as it fits the buffer it describes.
fn checked_count(moved: libc::c_int, length: usize) -> io::Result<usize> {
    usize::try_from(moved)
        .ok()
        .filter(|&moved| moved <= length)
        .ok_or_else(malformed)
}

/// Refuses an endpoint address the direction doesn't match, endpoint 0, and reserved bits.
fn check_endpoint(endpoint: u8, is_in: bool) -> io::Result<()> {
    let right_direction = (endpoint & DIR_IN != 0) == is_in;
    if !right_direction || endpoint & ENDPOINT_RESERVED != 0 || endpoint & ENDPOINT_NUMBER == 0 {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(())
}

/// The kernel reported something that can't be true of memory it was given.
fn malformed() -> io::Error {
    io::ErrorKind::InvalidData.into()
}

#[cfg(test)]
mod tests;
