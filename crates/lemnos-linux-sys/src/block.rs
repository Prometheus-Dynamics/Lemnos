//! The generic Lemnos block device, `/dev/lemnos-<device>` (`kernel-modules/lemnos`, driver
//! `lemnos`): its uAPI, `lemnos.h`, written for this crate by the same authors and reached only
//! through ioctl, read and mmap.
//!
//! A block Lemnos owns, claimed through `driver_override`, for a driver in user space: its
//! register regions ([`region`], [`map_region`] where [`Info::registers_mappable`]), DMA buffers
//! exported as dma-bufs ([`alloc`]), dma-bufs mapped to device addresses ([`import`],
//! [`release`]), and its interrupts ([`read_irqs`], [`irq_enable`]). Board-tested on the PiSP
//! back end's node (`kernel-modules/lemnos/README.md`).

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

use crate::ioctl::{ioctl_ptr, ior, iow, iowr};
use crate::mmap::Mapping;

/// `LEMNOS_ABI`: the uAPI revision this crate speaks.
pub const ABI: u32 = 1;
/// `LEMNOS_MAX_REGIONS`.
pub const MAX_REGIONS: u32 = 8;
/// `LEMNOS_MAX_IRQS`.
pub const MAX_IRQS: u32 = 4;

const INFO_MMIO_MAPPABLE: u32 = 1 << 0;

/// `struct lemnos_info`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Info {
    /// `LEMNOS_ABI` of the loaded module.
    pub abi: u32,
    /// `LEMNOS_INFO_*`; see [`Self::registers_mappable`].
    pub flags: u32,
    /// Register regions, up to [`MAX_REGIONS`].
    pub regions: u32,
    /// Interrupts forwarded, up to [`MAX_IRQS`].
    pub irqs: u32,
    /// Device addresses are below `2^dma_bits`.
    pub dma_bits: u32,
    reserved: [u32; 3],
}

impl Info {
    /// Whether [`map_region`] may map the registers: the block's DMA cannot reach memory the
    /// module did not map. Mapping also needs `CAP_SYS_RAWIO`.
    pub fn registers_mappable(&self) -> bool {
        self.flags & INFO_MMIO_MAPPABLE != 0
    }
}

/// `struct lemnos_region`: a register region's physical range.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Region {
    /// Its index, below [`Info::regions`].
    pub index: u32,
    reserved: u32,
    /// Physical address.
    pub start: u64,
    /// Bytes.
    pub size: u64,
}

/// `struct lemnos_alloc`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Alloc {
    size: u64,
    flags: u32,
    fd: i32,
    addr: u64,
}

/// `struct lemnos_import`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Import {
    fd: i32,
    handle: u32,
    addr: u64,
    size: u64,
}

/// `struct lemnos_irq_event`: what `read` returns per interrupt that fired.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IrqEvent {
    /// The interrupt's index, below [`Info::irqs`].
    pub irq: u32,
    /// Times it fired since the module bound.
    pub count: u32,
    /// `ktime_get_ns()` (`CLOCK_MONOTONIC`) when it last fired.
    pub ns: u64,
}

/// A DMA buffer from [`alloc`]: its dma-buf and device address.
#[derive(Debug)]
pub struct Buffer {
    /// The dma-buf (map it with [`Mapping::new`] for the CPU).
    pub fd: OwnedFd,
    /// The block's address for it.
    pub addr: u64,
    /// Bytes (the size asked for, rounded up to pages).
    pub size: u64,
}

/// A dma-buf mapped on the block by [`import`], until [`release`] or the file closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Imported {
    /// For [`release`]; never 0.
    pub handle: u32,
    /// The block's address for it.
    pub addr: u64,
    /// Bytes mapped.
    pub size: u64,
}

const IOC_INFO: u32 = ior::<Info>(b'L', 0xd0);
const IOC_REGION: u32 = iowr::<Region>(b'L', 0xd1);
const IOC_ALLOC: u32 = iowr::<Alloc>(b'L', 0xd2);
const IOC_IMPORT: u32 = iowr::<Import>(b'L', 0xd3);
const IOC_RELEASE: u32 = iow::<u32>(b'L', 0xd4);
const IOC_IRQ_ENABLE: u32 = iow::<u32>(b'L', 0xd5);

/// The device's info.
pub fn info(dev: BorrowedFd<'_>) -> io::Result<Info> {
    let mut info = Info::default();
    // SAFETY: `IOC_INFO` writes a `lemnos_info`, which `info` is.
    unsafe { ioctl_ptr(dev, IOC_INFO, &raw mut info)? };
    Ok(info)
}

/// Register region `index`'s physical range.
pub fn region(dev: BorrowedFd<'_>, index: u32) -> io::Result<Region> {
    let mut r = Region {
        index,
        ..Default::default()
    };
    // SAFETY: `IOC_REGION` reads and writes a `lemnos_region`, which `r` is.
    unsafe { ioctl_ptr(dev, IOC_REGION, &raw mut r)? };
    Ok(r)
}

/// Maps register region `index` (`len` bytes, from its start), writable. `EPERM` unless
/// [`Info::registers_mappable`] and the caller has `CAP_SYS_RAWIO`.
pub fn map_region(dev: BorrowedFd<'_>, index: u32, len: usize) -> io::Result<Mapping> {
    let page = crate::mmap::page_size();
    Mapping::new(dev, len, u64::from(index) * page as u64, true)
}

/// A DMA buffer of at least `size` bytes for the block, as a dma-buf (close-on-exec).
pub fn alloc(dev: BorrowedFd<'_>, size: u64) -> io::Result<Buffer> {
    let mut a = Alloc {
        size,
        ..Default::default()
    };
    // SAFETY: `IOC_ALLOC` reads and writes a `lemnos_alloc`, which `a` is.
    unsafe { ioctl_ptr(dev, IOC_ALLOC, &raw mut a)? };
    // SAFETY: the module installed `a.fd` as a new descriptor for this process, ours alone.
    let fd = unsafe { OwnedFd::from_raw_fd(a.fd) };
    Ok(Buffer {
        fd,
        addr: a.addr,
        size: size.next_multiple_of(crate::mmap::page_size() as u64),
    })
}

/// Maps `buf` on the block, contiguous in its address space (`EINVAL` if it cannot be). The
/// module keeps its own reference: `buf` is only borrowed for the call.
pub fn import(dev: BorrowedFd<'_>, buf: BorrowedFd<'_>) -> io::Result<Imported> {
    let mut i = Import {
        fd: buf.as_raw_fd(),
        ..Default::default()
    };
    // SAFETY: `IOC_IMPORT` reads and writes a `lemnos_import`, which `i` is.
    unsafe { ioctl_ptr(dev, IOC_IMPORT, &raw mut i)? };
    Ok(Imported {
        handle: i.handle,
        addr: i.addr,
        size: i.size,
    })
}

/// Unmaps an [`import`]; `ENOENT` for a handle not held.
pub fn release(dev: BorrowedFd<'_>, handle: u32) -> io::Result<()> {
    let mut h = handle;
    // SAFETY: `IOC_RELEASE` reads a `u32`, which `h` is.
    unsafe { ioctl_ptr(dev, IOC_RELEASE, &raw mut h)? };
    Ok(())
}

/// Unmasks interrupt `index` after the driver has cleared its cause; it masks itself again
/// when it next fires.
pub fn irq_enable(dev: BorrowedFd<'_>, index: u32) -> io::Result<()> {
    let mut i = index;
    // SAFETY: `IOC_IRQ_ENABLE` reads a `u32`, which `i` is.
    unsafe { ioctl_ptr(dev, IOC_IRQ_ENABLE, &raw mut i)? };
    Ok(())
}

/// Reads the interrupts that fired into `out`, returning how many. Blocks for the first unless
/// the descriptor is non-blocking (`WouldBlock` then when none has).
pub fn read_irqs(dev: BorrowedFd<'_>, out: &mut [IrqEvent]) -> io::Result<usize> {
    loop {
        // SAFETY: `out` is valid for `size_of_val(out)` bytes of writes, and `IrqEvent` is
        // plain data the module fills whole records of.
        let n = unsafe { libc::read(dev.as_raw_fd(), out.as_mut_ptr().cast(), size_of_val(out)) };
        if n >= 0 {
            return Ok(n as usize / size_of::<IrqEvent>());
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_and_requests_match_the_module_header() {
        assert_eq!(size_of::<Info>(), 32);
        assert_eq!(size_of::<Region>(), 24);
        assert_eq!(size_of::<Alloc>(), 24);
        assert_eq!(std::mem::offset_of!(Alloc, addr), 16);
        assert_eq!(size_of::<Import>(), 24);
        assert_eq!(size_of::<IrqEvent>(), 16);
        assert_eq!(IOC_INFO, 0x8020_4cd0);
        assert_eq!(IOC_REGION, 0xc018_4cd1);
        assert_eq!(IOC_ALLOC, 0xc018_4cd2);
        assert_eq!(IOC_IMPORT, 0xc018_4cd3);
        assert_eq!(IOC_RELEASE, 0x4004_4cd4);
        assert_eq!(IOC_IRQ_ENABLE, 0x4004_4cd5);
    }
}
