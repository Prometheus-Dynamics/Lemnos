//! usbdevfs (`/dev/bus/usb/BBB/DDD`): synchronous control and bulk transfers,
//! interface claims and alternate settings, and asynchronous URBs (bulk,
//! interrupt and isochronous) that the kernel completes in any order. Ported
//! from `<linux/usbdevice_fs.h>`.
//!
//! A [`UsbfsFile`] owns the URB memory it hands to the kernel: a buffer and its
//! header (with the isochronous descriptors after it) stay allocated from
//! submission until the completion is reaped or the caller takes it back, so
//! the kernel never writes to freed memory. Completions are matched by the
//! caller's tag, which the header carries in its `usercontext` field.

use crate::ioctl::{ioctl_int, ioctl_ptr};
use crate::mmap::Mapping;
use std::collections::HashMap;
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::mem::size_of;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

const USB: u8 = b'U';

const CONTROL: u32 = crate::iowr::<CtrlTransfer>(USB, 0);
const BULK: u32 = crate::iowr::<BulkTransfer>(USB, 2);
const SETINTERFACE: u32 = crate::ior::<SetInterface>(USB, 4);
const SETCONFIGURATION: u32 = crate::ior::<libc::c_uint>(USB, 5);
const GETDRIVER: u32 = crate::iow::<GetDriver>(USB, 8);
const SUBMITURB: u32 = crate::ior::<UrbHeader>(USB, 10);
const DISCARDURB: u32 = crate::ioc(0, USB, 11, 0);
const REAPURB: u32 = crate::iow::<*mut c_void>(USB, 12);
const REAPURBNDELAY: u32 = crate::iow::<*mut c_void>(USB, 13);
const CLAIMINTERFACE: u32 = crate::ior::<libc::c_uint>(USB, 15);
const RELEASEINTERFACE: u32 = crate::ior::<libc::c_uint>(USB, 16);
const CONNECT: u32 = crate::ioc(0, USB, 23, 0);
const RESET: u32 = crate::ioc(0, USB, 20, 0);
const CLEAR_HALT: u32 = crate::ior::<libc::c_uint>(USB, 21);
const GET_CAPABILITIES: u32 = crate::ior::<u32>(USB, 26);
const DISCONNECT_CLAIM: u32 = crate::ior::<DisconnectClaim>(USB, 27);
const GET_SPEED: u32 = crate::ioc(0, USB, 31, 0);

/// `USBDEVFS_URB_TYPE_*`.
const URB_TYPE_ISO: u8 = 0;
const URB_TYPE_INTERRUPT: u8 = 1;
const URB_TYPE_BULK: u8 = 3;
/// `USBDEVFS_URB_ISO_ASAP`: start isochronous URBs at the next free frame.
const URB_ISO_ASAP: u32 = 0x2;
/// `USBDEVFS_CAP_MMAP`: the device file can be mapped, and each mapping is
/// memory the URBs can use as DMA buffers (no copy into a kernel bounce buffer).
pub const CAP_MMAP: u32 = 0x20;

/// `USBDEVFS_URB_ZERO_PACKET`: an OUT bulk or interrupt URB whose length is a
/// multiple of the endpoint's packet size ends with a zero-length packet, so
/// the device sees the end of the transfer.
pub const URB_ZERO_PACKET: u32 = 0x40;

/// The URB flags for a submission. Isochronous URBs start at the next free
/// frame; the zero-length terminator applies to OUT bulk and interrupt URBs
/// only, so any other use is refused before the kernel sees it.
pub(crate) fn urb_flags(kind: UrbKind, endpoint: u8, zero_packet: bool) -> io::Result<u32> {
    let mut flags = if kind == UrbKind::Isochronous {
        URB_ISO_ASAP
    } else {
        0
    };
    if zero_packet {
        if kind == UrbKind::Isochronous || endpoint & 0x80 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a zero-length terminator applies to OUT bulk and interrupt transfers",
            ));
        }
        flags |= URB_ZERO_PACKET;
    }
    Ok(flags)
}

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
    driver: [u8; DRIVER_NAME_LEN],
}

/// `struct usbdevfs_disconnect_claim`.
#[repr(C)]
#[derive(Clone, Copy)]
struct DisconnectClaim {
    interface: u32,
    flags: u32,
    driver: [u8; DRIVER_NAME_LEN],
}

const DRIVER_NAME_LEN: usize = 256;

/// `struct usbdevfs_urb`: the header the kernel reads on submit and writes on
/// completion. Its isochronous descriptors follow it in memory.
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

const HEADER_LEN: usize = size_of::<UrbHeader>();
const ISO_DESC_LEN: usize = size_of::<IsoPacketDesc>();

/// Kind of an asynchronous transfer. Direction comes from the endpoint address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrbKind {
    /// Bulk transfer.
    Bulk,
    /// Interrupt transfer.
    Interrupt,
    /// Isochronous transfer: one packet per entry of the packet lengths.
    Isochronous,
}

/// The status of one isochronous packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsoPacket {
    /// The length asked for.
    pub length: u32,
    /// The bytes moved.
    pub actual_length: u32,
    /// Zero, or a negative errno.
    pub status: i32,
}

/// A finished asynchronous transfer, with its buffer back.
#[derive(Debug)]
pub struct UrbCompletion {
    /// The tag given at submission.
    pub tag: u64,
    /// The buffer submitted, holding the data an IN transfer received.
    pub buffer: UrbBuffer,
    /// Zero, or a negative errno.
    pub status: i32,
    /// The bytes moved.
    pub actual_length: usize,
    /// The isochronous packets (empty for bulk and interrupt).
    pub packets: Vec<IsoPacket>,
}

/// The memory a URB transfers to or from, owned by the caller until the URB
/// is submitted and by the table while it is in flight.
pub enum UrbBuffer {
    /// Heap memory. The kernel maps it for DMA for the transfer's duration.
    Heap(Vec<u8>),
    /// Memory from [`UsbfsFile::map_buffer`]: a usbdevfs mapping, page-aligned,
    /// usable directly as URB memory (`CAP_MMAP`). `len` is the bytes in use.
    Mapped {
        /// The mapping (a whole number of pages).
        mapping: Mapping,
        /// Bytes of the mapping the transfer uses.
        len: usize,
    },
}

impl std::fmt::Debug for UrbBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UrbBuffer")
            .field("len", &self.len())
            .field("mapped", &self.is_mapped())
            .finish()
    }
}

impl UrbBuffer {
    /// Bytes in use.
    pub fn len(&self) -> usize {
        match self {
            Self::Heap(bytes) => bytes.len(),
            Self::Mapped { len, .. } => *len,
        }
    }

    /// Whether no bytes are in use.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes available: the whole mapping, or the heap buffer's length.
    pub fn capacity(&self) -> usize {
        match self {
            Self::Heap(bytes) => bytes.len(),
            Self::Mapped { mapping, .. } => mapping.len(),
        }
    }

    /// Whether the memory is a usbdevfs mapping.
    pub fn is_mapped(&self) -> bool {
        matches!(self, Self::Mapped { .. })
    }

    /// The bytes in use.
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::Heap(bytes) => bytes,
            Self::Mapped { mapping, len } => &mapping.as_slice()[..*len],
        }
    }

    /// The bytes in use, for writing before submission.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        match self {
            Self::Heap(bytes) => bytes,
            Self::Mapped { mapping, len } => &mut mapping.as_mut_slice()[..*len],
        }
    }

    /// Sets the bytes in use. A heap buffer grows with zeros or shrinks; a
    /// mapping is clamped to its length.
    pub fn set_len(&mut self, new_len: usize) {
        match self {
            Self::Heap(bytes) => bytes.resize(new_len, 0),
            Self::Mapped { mapping, len } => *len = new_len.min(mapping.len()),
        }
    }

    /// The memory as a heap vector: the buffer itself, or a copy of a mapping's bytes.
    pub fn into_vec(self) -> Vec<u8> {
        match self {
            Self::Heap(bytes) => bytes,
            Self::Mapped { mapping, len } => mapping.as_slice()[..len].to_vec(),
        }
    }

    fn data_ptr(&mut self) -> *mut u8 {
        match self {
            Self::Heap(bytes) => bytes.as_mut_ptr(),
            Self::Mapped { mapping, .. } => mapping.as_mut_slice().as_mut_ptr(),
        }
    }
}

/// The bytes a usbdevfs mapping of `len` bytes needs: `len` rounded up to whole
/// pages (`page` bytes each). A zero length still takes one page.
pub fn mapping_len(len: usize, page: usize) -> usize {
    let page = page.max(1);
    len.max(1).div_ceil(page) * page
}

/// The URB memory the kernel holds for one submission.
struct InFlight {
    /// The header, then the isochronous descriptors, in 8-byte words.
    block: Vec<u64>,
    buffer: UrbBuffer,
    packets: usize,
}

/// Header blocks kept for reuse: a steady stream of submissions allocates none.
const SPARE_BLOCKS: usize = 16;

/// An open usbdevfs device file, and the URBs in flight on it.
pub struct UsbfsFile {
    file: File,
    urbs: Mutex<HashMap<u64, InFlight>>,
    spare: Mutex<Vec<Vec<u64>>>,
}

impl UsbfsFile {
    /// Opens a device node read-write (`/dev/bus/usb/BBB/DDD`).
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self {
            file,
            urbs: Mutex::new(HashMap::new()),
            spare: Mutex::new(Vec::new()),
        })
    }

    /// The file descriptor, for polling.
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }

    /// The raw descriptors the kernel reports for the device: its device
    /// descriptor, then each configuration descriptor, as one blob.
    pub fn read_descriptors(&self) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; 64 * 1024];
        let read = (&self.file).read(&mut buf)?;
        buf.truncate(read);
        Ok(buf)
    }

    /// A control IN transfer (`bRequestType` bit 7 set); fills `data` and
    /// returns the bytes received.
    pub fn control_in(
        &self,
        setup: ControlSetup,
        data: &mut [u8],
        timeout_ms: u32,
    ) -> io::Result<usize> {
        self.control(setup, data.as_mut_ptr().cast(), data.len(), timeout_ms)
    }

    /// A control OUT transfer; returns the bytes sent.
    pub fn control_out(
        &self,
        setup: ControlSetup,
        data: &[u8],
        timeout_ms: u32,
    ) -> io::Result<usize> {
        // The kernel only reads `data` for an OUT request, so the pointer is
        // never used to write.
        self.control(
            setup,
            data.as_ptr().cast_mut().cast(),
            data.len(),
            timeout_ms,
        )
    }

    fn control(
        &self,
        setup: ControlSetup,
        data: *mut c_void,
        length: usize,
        timeout_ms: u32,
    ) -> io::Result<usize> {
        let length = u16::try_from(length).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "control data longer than 65535 bytes",
            )
        })?;
        let mut arg = CtrlTransfer {
            request_type: setup.request_type,
            request: setup.request,
            value: setup.value,
            index: setup.index,
            length,
            timeout: timeout_ms,
            data,
        };
        // SAFETY: `arg` is a live `usbdevfs_ctrltransfer`; `data` points to
        // `length` bytes that outlive this synchronous call.
        let n = unsafe { ioctl_ptr(self.file.as_fd(), CONTROL, &mut arg) }?;
        Ok(n as usize)
    }

    /// A bulk or interrupt IN transfer into `data` (endpoint with bit 7 set).
    pub fn bulk_in(&self, endpoint: u8, data: &mut [u8], timeout_ms: u32) -> io::Result<usize> {
        self.bulk(endpoint, data.as_mut_ptr().cast(), data.len(), timeout_ms)
    }

    /// A bulk or interrupt OUT transfer of `data`.
    pub fn bulk_out(&self, endpoint: u8, data: &[u8], timeout_ms: u32) -> io::Result<usize> {
        self.bulk(
            endpoint,
            data.as_ptr().cast_mut().cast(),
            data.len(),
            timeout_ms,
        )
    }

    fn bulk(
        &self,
        endpoint: u8,
        data: *mut c_void,
        length: usize,
        timeout_ms: u32,
    ) -> io::Result<usize> {
        let length = u32::try_from(length).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "bulk data longer than 4 GiB")
        })?;
        let mut arg = BulkTransfer {
            endpoint: u32::from(endpoint),
            length,
            timeout: timeout_ms,
            data,
        };
        // SAFETY: `arg` is a live `usbdevfs_bulktransfer`; `data` points to
        // `length` bytes that outlive this synchronous call.
        let n = unsafe { ioctl_ptr(self.file.as_fd(), BULK, &mut arg) }?;
        Ok(n as usize)
    }

    /// Claims interface `number`. With `detach`, the kernel driver bound to it is
    /// unbound in the same step (`USBDEVFS_DISCONNECT_CLAIM`).
    pub fn claim_interface(&self, number: u8, detach: bool) -> io::Result<()> {
        if detach {
            let mut arg = DisconnectClaim {
                interface: u32::from(number),
                flags: 0,
                driver: [0; DRIVER_NAME_LEN],
            };
            // SAFETY: `arg` is a live `usbdevfs_disconnect_claim` the kernel reads.
            unsafe { ioctl_ptr(self.file.as_fd(), DISCONNECT_CLAIM, &mut arg) }?;
            return Ok(());
        }
        let mut n = libc::c_uint::from(number);
        // SAFETY: `n` is a live `unsigned int` the kernel reads.
        unsafe { ioctl_ptr(self.file.as_fd(), CLAIMINTERFACE, &mut n) }?;
        Ok(())
    }

    /// Releases interface `number`; reattaches the kernel driver after a detaching claim.
    pub fn release_interface(&self, number: u8, reattach: bool) -> io::Result<()> {
        let mut n = libc::c_uint::from(number);
        // SAFETY: `n` is a live `unsigned int` the kernel reads.
        unsafe { ioctl_ptr(self.file.as_fd(), RELEASEINTERFACE, &mut n) }?;
        if reattach {
            // Best effort: the interface may have no driver to return to.
            let _ = ioctl_int(self.file.as_fd(), CONNECT, 0);
        }
        Ok(())
    }

    /// Selects alternate setting `alternate` of interface `number`.
    pub fn set_interface(&self, number: u8, alternate: u8) -> io::Result<()> {
        let mut arg = SetInterface {
            interface: u32::from(number),
            alternate: u32::from(alternate),
        };
        // SAFETY: `arg` is a live `usbdevfs_setinterface` the kernel reads.
        unsafe { ioctl_ptr(self.file.as_fd(), SETINTERFACE, &mut arg) }?;
        Ok(())
    }

    /// Selects configuration `value`.
    pub fn set_configuration(&self, value: u8) -> io::Result<()> {
        let mut v = libc::c_uint::from(value);
        // SAFETY: `v` is a live `unsigned int` the kernel reads.
        unsafe { ioctl_ptr(self.file.as_fd(), SETCONFIGURATION, &mut v) }?;
        Ok(())
    }

    /// The name of the kernel driver bound to interface `number`, if any.
    pub fn driver(&self, number: u8) -> io::Result<Option<String>> {
        let mut arg = GetDriver {
            interface: u32::from(number),
            driver: [0; DRIVER_NAME_LEN],
        };
        // SAFETY: `arg` is a live `usbdevfs_getdriver`; the kernel writes the name into it.
        match unsafe { ioctl_ptr(self.file.as_fd(), GETDRIVER, &mut arg) } {
            Ok(_) => {
                let end = arg
                    .driver
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(DRIVER_NAME_LEN);
                Ok(Some(
                    String::from_utf8_lossy(&arg.driver[..end]).into_owned(),
                ))
            }
            Err(error) if error.raw_os_error() == Some(libc::ENODATA) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Clears the halt condition on `endpoint`.
    pub fn clear_halt(&self, endpoint: u8) -> io::Result<()> {
        let mut ep = libc::c_uint::from(endpoint);
        // SAFETY: `ep` is a live `unsigned int` the kernel reads.
        unsafe { ioctl_ptr(self.file.as_fd(), CLEAR_HALT, &mut ep) }?;
        Ok(())
    }

    /// Resets the device.
    pub fn reset(&self) -> io::Result<()> {
        ioctl_int(self.file.as_fd(), RESET, 0)?;
        Ok(())
    }

    /// The device's speed, as the kernel's `enum usb_device_speed` number.
    pub fn speed(&self) -> io::Result<i32> {
        ioctl_int(self.file.as_fd(), GET_SPEED, 0)
    }

    /// The `USBDEVFS_CAP_*` bits the kernel supports.
    pub fn capabilities(&self) -> io::Result<u32> {
        let mut caps: u32 = 0;
        // SAFETY: `caps` is a live `u32` the kernel writes.
        unsafe { ioctl_ptr(self.file.as_fd(), GET_CAPABILITIES, &mut caps) }?;
        Ok(caps)
    }

    /// Submits an asynchronous transfer. `buffer` is sent (OUT) or receives
    /// the data (IN); for isochronous transfers `packet_lengths` gives each
    /// packet's length and their sum is the buffer's length. `tag` identifies
    /// the completion; it must not be in flight already. `zero_packet` sets
    /// [`URB_ZERO_PACKET`] (OUT bulk and interrupt only).
    pub fn submit(
        &self,
        tag: u64,
        kind: UrbKind,
        endpoint: u8,
        mut buffer: UrbBuffer,
        packet_lengths: &[u32],
        zero_packet: bool,
    ) -> io::Result<()> {
        let flags = urb_flags(kind, endpoint, zero_packet)?;
        let packets = match kind {
            UrbKind::Isochronous => packet_lengths.len(),
            UrbKind::Bulk | UrbKind::Interrupt => 0,
        };
        if kind == UrbKind::Isochronous {
            let total: u64 = packet_lengths.iter().map(|&len| u64::from(len)).sum();
            if packets == 0 || total != buffer.len() as u64 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "isochronous packets must be non-empty and sum to the buffer length",
                ));
            }
        }
        let packets_i32 = i32::try_from(packets).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "too many isochronous packets")
        })?;
        let buffer_length = i32::try_from(buffer.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "transfer longer than 2 GiB")
        })?;

        let words = (HEADER_LEN + packets * ISO_DESC_LEN).div_ceil(size_of::<u64>());
        let mut block = self.take_block(words);
        let base = block.as_mut_ptr().cast::<u8>();
        let header = UrbHeader {
            kind: match kind {
                UrbKind::Bulk => URB_TYPE_BULK,
                UrbKind::Interrupt => URB_TYPE_INTERRUPT,
                UrbKind::Isochronous => URB_TYPE_ISO,
            },
            endpoint,
            status: 0,
            flags,
            buffer: buffer.data_ptr().cast(),
            buffer_length,
            actual_length: 0,
            start_frame: 0,
            number_of_packets: packets_i32,
            error_count: 0,
            signr: 0,
            usercontext: tag as usize as *mut c_void,
        };
        // SAFETY: `block` holds `HEADER_LEN` bytes at its start (its length is
        // rounded up to whole words, and the header's alignment is 8).
        unsafe { base.cast::<UrbHeader>().write(header) };
        for (index, &length) in packet_lengths.iter().enumerate() {
            let desc = IsoPacketDesc {
                length,
                actual_length: 0,
                status: 0,
            };
            // SAFETY: `block` has room for `packets` descriptors after the
            // header; `index` < `packets`, so the offset stays inside it.
            let slot =
                unsafe { base.add(HEADER_LEN + index * ISO_DESC_LEN) }.cast::<IsoPacketDesc>();
            // SAFETY: `slot` is the 4-aligned descriptor slot just computed, inside `block`.
            unsafe { slot.write(desc) };
        }
        let header_ptr = base.cast::<UrbHeader>();

        // The table owns `block` and `buffer` from here on: moving the Vecs
        // does not move their heap memory, which the kernel now points at.
        {
            let mut urbs = self.lock_urbs();
            if urbs.contains_key(&tag) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "URB tag already in flight",
                ));
            }
            urbs.insert(
                tag,
                InFlight {
                    block,
                    buffer,
                    packets,
                },
            );
        }
        // SAFETY: `header_ptr` points at the header inside the table's block,
        // which stays allocated until the URB is reaped or removed below.
        if let Err(error) = unsafe { ioctl_ptr(self.file.as_fd(), SUBMITURB, header_ptr) } {
            self.lock_urbs().remove(&tag);
            return Err(error);
        }
        Ok(())
    }

    /// A zeroed header block of `words` words, from the spare list when there is one.
    fn take_block(&self, words: usize) -> Vec<u64> {
        let mut block = self
            .spare
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop()
            .unwrap_or_default();
        block.clear();
        block.resize(words, 0);
        block
    }

    fn give_block(&self, block: Vec<u64>) {
        let mut spare = self
            .spare
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if spare.len() < SPARE_BLOCKS {
            spare.push(block);
        }
    }

    /// Whether the kernel lets this device file map DMA buffers (`CAP_MMAP`).
    pub fn has_mmap(&self) -> io::Result<bool> {
        Ok(self.capabilities()? & CAP_MMAP != 0)
    }

    /// Maps a buffer of `len` bytes (rounded up to whole pages) for URBs. The
    /// kernel allocates the memory with each mapping; it is released when the
    /// mapping is dropped, which the table never does while a URB uses it.
    pub fn map_buffer(&self, len: usize) -> io::Result<UrbBuffer> {
        // SAFETY: `sysconf` takes no pointers and only reads the page size.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let page = usize::try_from(page).unwrap_or(4096);
        let mapped = mapping_len(len, page);
        let mapping = Mapping::new(self.file.as_fd(), mapped, 0, true)?;
        Ok(UrbBuffer::Mapped { mapping, len })
    }

    /// Waits for the next completed URB (`blocking`) or returns `None` at once
    /// when none has completed.
    pub fn reap(&self, blocking: bool) -> io::Result<Option<UrbCompletion>> {
        let mut urb: *mut c_void = std::ptr::null_mut();
        let request = if blocking { REAPURB } else { REAPURBNDELAY };
        // SAFETY: `urb` is a live pointer-sized slot the kernel writes the
        // completed header's address into.
        match unsafe { ioctl_ptr(self.file.as_fd(), request, &mut urb) } {
            Ok(_) => {}
            Err(error) if !blocking && error.raw_os_error() == Some(libc::EAGAIN) => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        }
        let header = urb.cast::<UrbHeader>().cast_const();
        // SAFETY: the kernel returns the address of a header this file submitted
        // and has not yet reaped; the table still owns that memory.
        let tag = unsafe { (*header).usercontext } as usize as u64;
        let in_flight = self.lock_urbs().remove(&tag).ok_or_else(|| {
            io::Error::other("the kernel completed a URB this file did not submit")
        })?;
        // SAFETY: the URB is complete (the kernel has released it), so its header
        // is no longer written; it is the memory `in_flight.block` starts with.
        let status = unsafe { (*header).status };
        // SAFETY: as above.
        let actual_length = unsafe { (*header).actual_length };
        let packets = read_packets(&in_flight);
        let InFlight { block, buffer, .. } = in_flight;
        self.give_block(block);
        Ok(Some(UrbCompletion {
            tag,
            buffer,
            status,
            actual_length: usize::try_from(actual_length).unwrap_or(0),
            packets,
        }))
    }

    /// Cancels the URB with `tag`. Its completion still arrives, with a negative status.
    pub fn discard(&self, tag: u64) -> io::Result<()> {
        let address = {
            let urbs = self.lock_urbs();
            match urbs.get(&tag) {
                Some(in_flight) => in_flight.block.as_ptr() as usize,
                None => return Ok(()),
            }
        };
        match ioctl_int(self.file.as_fd(), DISCARDURB, address as libc::c_ulong) {
            Ok(_) => Ok(()),
            // Already completed: its completion is waiting to be reaped.
            Err(error) if error.raw_os_error() == Some(libc::EINVAL) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// The number of URBs submitted and not yet reaped.
    pub fn in_flight(&self) -> usize {
        self.lock_urbs().len()
    }

    fn lock_urbs(&self) -> MutexGuard<'_, HashMap<u64, InFlight>> {
        self.urbs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The isochronous packet statuses of a completed URB.
fn read_packets(in_flight: &InFlight) -> Vec<IsoPacket> {
    let mut packets = Vec::with_capacity(in_flight.packets);
    for index in 0..in_flight.packets {
        let base = in_flight.block.as_ptr().cast::<u8>();
        // SAFETY: the completed URB's descriptors are in `in_flight.block`, one
        // per packet, after the header; `index` < `packets`, so the offset is inside it.
        let slot = unsafe { base.add(HEADER_LEN + index * ISO_DESC_LEN) }.cast::<IsoPacketDesc>();
        // SAFETY: `slot` is one of those descriptors, written at submission and not since.
        let desc = unsafe { slot.read() };
        packets.push(IsoPacket {
            length: desc.length,
            actual_length: desc.actual_length,
            status: desc.status as i32,
        });
    }
    packets
}

/// A control request's setup fields.
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

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;

    #[test]
    fn mapped_buffers_round_up_to_whole_pages_and_the_heap_grows() {
        assert_eq!(CAP_MMAP, 0x20);
        assert_eq!(
            mapping_len(0, 4096),
            4096,
            "a zero length still takes a page"
        );
        assert_eq!(mapping_len(1, 4096), 4096);
        assert_eq!(mapping_len(4096, 4096), 4096);
        assert_eq!(mapping_len(4097, 4096), 8192);
        assert_eq!(
            mapping_len(10, 0),
            10,
            "a zero page size is treated as one byte"
        );
        let mut heap = UrbBuffer::Heap(vec![7; 4]);
        assert_eq!(heap.len(), 4);
        assert!(!heap.is_mapped());
        heap.set_len(6);
        assert_eq!(heap.as_slice(), &[7, 7, 7, 7, 0, 0]);
        heap.set_len(1);
        assert_eq!(heap.into_vec(), vec![7]);
    }

    #[test]
    fn urb_flags_carry_the_zero_packet_bit_on_out_bulk_and_interrupt_only() {
        assert_eq!(URB_ZERO_PACKET, 0x40);
        assert_eq!(urb_flags(UrbKind::Bulk, 0x01, false).unwrap(), 0);
        assert_eq!(urb_flags(UrbKind::Bulk, 0x01, true).unwrap(), 0x40);
        assert_eq!(urb_flags(UrbKind::Interrupt, 0x02, true).unwrap(), 0x40);
        assert_eq!(urb_flags(UrbKind::Isochronous, 0x81, false).unwrap(), 0x2);
        // IN endpoints and isochronous URBs cannot take the terminator.
        assert!(urb_flags(UrbKind::Bulk, 0x81, true).is_err());
        assert!(urb_flags(UrbKind::Isochronous, 0x81, true).is_err());
    }

    #[test]
    fn structures_match_the_kernel_layout_on_64_bit() {
        assert_eq!(size_of::<CtrlTransfer>(), 24);
        assert_eq!(size_of::<BulkTransfer>(), 24);
        assert_eq!(size_of::<SetInterface>(), 8);
        assert_eq!(size_of::<GetDriver>(), 260);
        assert_eq!(size_of::<DisconnectClaim>(), 264);
        assert_eq!(size_of::<IsoPacketDesc>(), 12);
        assert_eq!(size_of::<UrbHeader>(), 56);
        assert_eq!(HEADER_LEN, 56);
    }

    #[test]
    fn request_numbers_match_the_kernel() {
        assert_eq!(CONTROL, 0xc018_5500);
        assert_eq!(BULK, 0xc018_5502);
        assert_eq!(SUBMITURB, 0x8038_550a);
        assert_eq!(DISCARDURB, 0x0000_550b);
        assert_eq!(REAPURB, 0x4008_550c);
        assert_eq!(CLAIMINTERFACE, 0x8004_550f);
        assert_eq!(DISCONNECT_CLAIM, 0x8108_551b);
        assert_eq!(GET_SPEED, 0x0000_551f);
    }
}
