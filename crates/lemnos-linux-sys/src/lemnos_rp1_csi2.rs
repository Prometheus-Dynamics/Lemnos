//! Lemnos's own RP1 CSI-2 receiver module, `/dev/lemnos-rp1-csi2` (`kernel-modules/lemnos`):
//! its uAPI, `lemnos_rp1_csi2.h`, written for this crate by the same authors and reached only
//! through ioctl and read.
//!
//! The sensor is the caller's: the module only receives. [`start`] gives it the link the sensor
//! was set up for and the stream to keep; [`queue`] lends it a dma-buf (one contiguous segment:
//! RP1 has no IOMMU) to write the next frame into; [`read_done`] returns one [`Done`] per buffer,
//! in order. The module holds its own reference to each buffer until it is read back.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};

use crate::ioctl::{ioc, ioctl_int, ioctl_ptr, ior, iow};

/// `LEMNOS_RP1_CSI2_ABI`: the uAPI revision this crate speaks.
pub const ABI: u32 = 1;
/// `LEMNOS_RP1_CSI2_DEPTH`: buffers that may be queued and not yet read.
pub const DEPTH: usize = 8;
/// `LEMNOS_RP1_CSI2_PACK_BYTES`: write lines packed, without stride padding.
pub const PACK_BYTES: u32 = 1;
/// `LEMNOS_RP1_CSI2_UNPACK16`: unpack each RAW10/12/14 pixel to 16 bits, most significant bit
/// aligned, which is what the PiSP back end reads. Needs a width, a stride of at least twice it,
/// and not [`PACK_BYTES`].
pub const UNPACK16: u32 = 2;

/// `struct lemnos_rp1_csi2_info`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Info {
    /// `LEMNOS_RP1_CSI2_ABI` of the loaded module.
    pub abi: u32,
    /// The CSI-2 host's version register.
    pub host_version: u32,
    /// Data lanes wired, from the device tree.
    pub max_lanes: u32,
    /// [`DEPTH`] of the loaded module.
    pub depth: u32,
    /// Nonzero between [`start`] and [`stop`].
    pub streaming: u32,
    /// Frame starts since [`start`].
    pub frames: u32,
    /// Frame starts with no buffer to write into.
    pub dropped: u32,
    /// Zero.
    pub reserved: u32,
}

/// `struct lemnos_rp1_csi2_start`: the link and the stream.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Start {
    /// Data lanes, 1 to [`Info::max_lanes`].
    pub lanes: u32,
    /// Per-lane rate in Mbit/s, 80 to 1500: the link frequency times 2.
    pub mbps: u32,
    /// Virtual channel, 0 to 3.
    pub vc: u32,
    /// CSI-2 data type (`0x2b`: RAW10), 1 to 63.
    pub dt: u32,
    /// Pixels per line; with `height`, the frame-size check (0: none).
    pub width: u32,
    /// Lines.
    pub height: u32,
    /// Bytes per line in the buffer, a multiple of 16.
    pub stride: u32,
    /// Bytes written at most, a multiple of 16.
    pub size: u32,
    /// [`PACK_BYTES`], [`UNPACK16`].
    pub flags: u32,
    /// Zero ([`start`] clears it).
    pub reserved: [u32; 3],
}

/// `struct lemnos_rp1_csi2_queue`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct Queue {
    fd: i32,
    offset: u32,
    tag: u64,
}

/// `struct lemnos_rp1_csi2_done`: what `read` returns for each buffer.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Done {
    /// The tag the buffer was queued with.
    pub tag: u64,
    /// `ktime_get_ns()` (`CLOCK_MONOTONIC`) at the frame start.
    pub fs_ns: u64,
    /// The same at the frame end (0 if none came).
    pub fe_ns: u64,
    /// Frame starts before this one since [`start`].
    pub sequence: u32,
    /// 0; `-EIO` when a frame start came before its end; `-ECANCELED` when returned by [`stop`]
    /// or close.
    pub status: i32,
}

const IOC_INFO: u32 = ior::<Info>(b'L', 0xc0);
const IOC_START: u32 = iow::<Start>(b'L', 0xc1);
const IOC_QUEUE: u32 = iow::<Queue>(b'L', 0xc2);
const IOC_STOP: u32 = ioc(0, b'L', 0xc3, 0);

/// The module's info.
pub fn info(dev: BorrowedFd<'_>) -> io::Result<Info> {
    let mut info = Info::default();
    // SAFETY: `IOC_INFO` writes a `lemnos_rp1_csi2_info`, which `info` is.
    unsafe { ioctl_ptr(dev, IOC_INFO, &raw mut info)? };
    Ok(info)
}

/// Starts receiving. `EINVAL` for a link or stream outside the ranges on [`Start`]; `EBUSY` when
/// already started.
pub fn start(dev: BorrowedFd<'_>, start: &Start) -> io::Result<()> {
    let mut req = Start {
        reserved: [0; 3],
        ..*start
    };
    // SAFETY: `IOC_START` reads a `lemnos_rp1_csi2_start`, which `req` is.
    unsafe { ioctl_ptr(dev, IOC_START, &raw mut req)? };
    Ok(())
}

/// Lends the module `buf` (from byte `offset`, a multiple of 16) for one frame. `ENODATA` before
/// [`start`]; `EBUSY` when [`DEPTH`] buffers are queued and not yet read; `EINVAL` for a buffer
/// that is not one contiguous segment or too small for [`Start::size`].
pub fn queue(dev: BorrowedFd<'_>, buf: BorrowedFd<'_>, offset: u32, tag: u64) -> io::Result<()> {
    let mut req = Queue {
        fd: buf.as_raw_fd(),
        offset,
        tag,
    };
    // SAFETY: `IOC_QUEUE` reads a `lemnos_rp1_csi2_queue`, which `req` is. The descriptor is
    // borrowed for the call; the module takes its own reference.
    unsafe { ioctl_ptr(dev, IOC_QUEUE, &raw mut req)? };
    Ok(())
}

/// Stops receiving; every buffer not yet read comes back with `-ECANCELED`.
pub fn stop(dev: BorrowedFd<'_>) -> io::Result<()> {
    ioctl_int(dev, IOC_STOP, 0).map(|_| ())
}

/// Reads finished buffers into `out`, returning how many. Blocks for the first unless the
/// descriptor is non-blocking (`WouldBlock` then when none has finished).
pub fn read_done(dev: BorrowedFd<'_>, out: &mut [Done]) -> io::Result<usize> {
    loop {
        // SAFETY: `out` is valid for `size_of_val(out)` bytes of writes, and `Done` is plain
        // data the module fills whole records of.
        let n = unsafe { libc::read(dev.as_raw_fd(), out.as_mut_ptr().cast(), size_of_val(out)) };
        if n >= 0 {
            return Ok(n as usize / size_of::<Done>());
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
        assert_eq!(size_of::<Start>(), 48);
        assert_eq!(size_of::<Queue>(), 16);
        assert_eq!(std::mem::offset_of!(Queue, tag), 8);
        assert_eq!(size_of::<Done>(), 32);
        assert_eq!(std::mem::offset_of!(Done, status), 28);
        assert_eq!(IOC_INFO, 0x8020_4cc0);
        assert_eq!(IOC_START, 0x4030_4cc1);
        assert_eq!(IOC_QUEUE, 0x4010_4cc2);
        assert_eq!(IOC_STOP, 0x0000_4cc3);
    }
}
