//! Lemnos's own PiSP back end module, `/dev/lemnos-pisp-be` (`kernel-modules/lemnos-pisp-be`):
//! its uAPI, `lemnos_pisp_be.h`, written for this crate by the same authors and reached only
//! through ioctl and read.
//!
//! A job is a `struct pisp_be_tiles_config` plus one dma-buf per back end address. The module
//! copies the configuration, maps each dma-buf on the back end itself and writes every address;
//! no address comes from user space. [`submit`] borrows the descriptors only for the call: the
//! module holds its own references until the job is read back.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};

use crate::ioctl::{ioctl_ptr, ior, iow, iowr};

/// `LEMNOS_PISP_BE_ABI`: the uAPI revision this crate speaks.
pub const ABI: u32 = 1;
/// `LEMNOS_PISP_BE_PLANES`: the back end's addresses, in register order.
pub const PLANES: usize = 13;
/// The main input's first plane (`LEMNOS_PISP_BE_PLANE_INPUT`); planes 1 and 2 follow.
pub const PLANE_INPUT: usize = 0;
/// The TDN input.
pub const PLANE_TDN_INPUT: usize = 3;
/// The stitch input.
pub const PLANE_STITCH_INPUT: usize = 4;
/// The TDN output.
pub const PLANE_TDN_OUTPUT: usize = 5;
/// The stitch output.
pub const PLANE_STITCH_OUTPUT: usize = 6;

/// Output `n`'s first plane (`LEMNOS_PISP_BE_PLANE_OUTPUT(n)`); planes 2 and 3 follow.
pub const fn plane_output(n: usize) -> usize {
    7 + 3 * n
}

/// `sizeof(struct pisp_be_tiles_config)`: the bytes [`submit`] takes as the job.
pub const TILES_CONFIG_LEN: usize = 16720;

/// `struct lemnos_pisp_be_plane`: a dma-buf and the plane's offset in it; `fd` -1 for none.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Plane {
    fd: i32,
    offset: u32,
}

/// `struct lemnos_pisp_be_info`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Info {
    /// `LEMNOS_PISP_BE_ABI` of the loaded module.
    pub abi: u32,
    /// The block's version register.
    pub hw_version: u32,
    /// Jobs that may be submitted and not yet read.
    pub queue_depth: u32,
    /// Nonzero after a job timed out: the block stopped.
    pub dead: u32,
}

/// `struct lemnos_pisp_be_submit`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct Submit {
    config: u64,
    tag: u64,
    planes: [Plane; PLANES],
}

/// `struct lemnos_pisp_be_done`: what `read` returns for each finished job.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Done {
    /// The tag the job was submitted with.
    pub tag: u64,
    /// `ktime_get_ns()` (`CLOCK_MONOTONIC`) in the interrupt that ended it.
    pub irq_ns: u64,
    /// 0, or `-ETIMEDOUT` when the block stopped.
    pub status: i32,
    /// The batch status register at that interrupt.
    pub batch_status: u32,
    /// The interrupt status the handler cleared.
    pub int_status: u32,
    /// The status register at that interrupt.
    pub block_status: u32,
}

/// A soft-recovery attempt (`LEMNOS_PISP_BE_RECOVER_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoverMode {
    /// Rewrite the AXI register and clear the interrupts.
    Axi = 1,
    /// Change the clock's rate and back.
    Clock = 2,
}

/// `struct lemnos_pisp_be_recover`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Recover {
    /// The [`RecoverMode`] tried.
    pub mode: u32,
    /// The status register afterwards.
    pub status: u32,
    /// The batch status register afterwards.
    pub batch_status: u32,
    /// Nonzero when the block was idle afterwards with done equal to started.
    pub revived: u32,
}

const IOC_INFO: u32 = ior::<Info>(b'L', 0xb0);
const IOC_SUBMIT: u32 = iow::<Submit>(b'L', 0xb1);
const IOC_RECOVER: u32 = iowr::<Recover>(b'L', 0xb2);

/// The module's info.
pub fn info(dev: BorrowedFd<'_>) -> io::Result<Info> {
    let mut info = Info::default();
    // SAFETY: `IOC_INFO` writes a `lemnos_pisp_be_info`, which `info` is.
    unsafe { ioctl_ptr(dev, IOC_INFO, &raw mut info)? };
    Ok(info)
}

/// Submits a job: `config`, the bytes of a `pisp_be_tiles_config`, and per address a dma-buf
/// with the plane's byte offset (a multiple of 16) in it. `EBUSY` when as many jobs as
/// [`Info::queue_depth`] are submitted and not yet read; `EIO` once the block stopped.
pub fn submit(
    dev: BorrowedFd<'_>,
    config: &[u8],
    tag: u64,
    planes: &[Option<(BorrowedFd<'_>, u32)>; PLANES],
) -> io::Result<()> {
    if config.len() != TILES_CONFIG_LEN {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let mut req = Submit {
        config: config.as_ptr() as u64,
        tag,
        planes: [Plane { fd: -1, offset: 0 }; PLANES],
    };
    for (p, plane) in req.planes.iter_mut().zip(planes) {
        if let Some((fd, offset)) = plane {
            *p = Plane {
                fd: fd.as_raw_fd(),
                offset: *offset,
            };
        }
    }
    // SAFETY: `IOC_SUBMIT` reads a `lemnos_pisp_be_submit`, which `req` is, and through its
    // `config` field `TILES_CONFIG_LEN` bytes, which `config` holds and lends for the call. The
    // descriptors are borrowed for the call; the module takes its own references.
    unsafe { ioctl_ptr(dev, IOC_SUBMIT, &raw mut req)? };
    Ok(())
}

/// Reads finished jobs into `out`, returning how many. Blocks for the first unless the
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

/// Tries one soft recovery on a stopped block.
pub fn recover(dev: BorrowedFd<'_>, mode: RecoverMode) -> io::Result<Recover> {
    let mut r = Recover {
        mode: mode as u32,
        ..Default::default()
    };
    // SAFETY: `IOC_RECOVER` reads and writes a `lemnos_pisp_be_recover`, which `r` is.
    unsafe { ioctl_ptr(dev, IOC_RECOVER, &raw mut r)? };
    Ok(r)
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    #[test]
    fn layouts_and_requests_match_the_module_header() {
        assert_eq!(size_of::<Plane>(), 8);
        assert_eq!(size_of::<Info>(), 16);
        assert_eq!(size_of::<Submit>(), 16 + 8 * PLANES);
        assert_eq!(std::mem::offset_of!(Submit, planes), 16);
        assert_eq!(size_of::<Done>(), 32);
        assert_eq!(std::mem::offset_of!(Done, status), 16);
        assert_eq!(size_of::<Recover>(), 16);
        assert_eq!(IOC_INFO, 0x8010_4cb0);
        assert_eq!(IOC_SUBMIT, 0x4078_4cb1);
        assert_eq!(IOC_RECOVER, 0xc010_4cb2);
        assert_eq!(plane_output(1), 10);
    }

    #[test]
    fn a_config_of_the_wrong_size_is_refused_before_the_call() {
        let null = std::fs::File::open("/dev/null").unwrap();
        let planes = [None; PLANES];
        let err = submit(null.as_fd(), &[0; 16], 0, &planes).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
