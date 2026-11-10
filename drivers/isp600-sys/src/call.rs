//! The four requests, on a subdevice's descriptor. Every pointer the kernel follows is built here
//! from a borrow of exactly the size the request names.

use core::ffi::c_void;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};

use crate::{
    H3aConfig, LOAD_BYTES, SAVE_LOAD_STATISTICS_BYTES, STATISTICS_BYTES, StatData, TableRegMap,
    VIDIOC_VIN_ISP_H3A_CFG, VIDIOC_VIN_ISP_LOAD_REG, VIDIOC_VIN_ISP_STAT_EN,
    VIDIOC_VIN_ISP_STAT_REQ,
};

/// Repeats `ioctl` while it is interrupted, and turns a failure into the error it set.
fn retry(mut ioctl: impl FnMut() -> libc::c_int) -> io::Result<()> {
    loop {
        if ioctl() >= 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// The load request's argument for `image`: its first byte and its whole length.
pub(crate) fn load_arg(image: &[u8; LOAD_BYTES]) -> TableRegMap {
    TableRegMap {
        // The kernel only reads through it.
        addr: image.as_ptr().cast_mut().cast::<c_void>(),
        size: LOAD_BYTES as u32,
    }
}

/// The statistics request's argument for `into`: its first byte and its whole length.
pub(crate) fn stat_arg(into: &mut [u8; STATISTICS_BYTES]) -> StatData {
    StatData {
        buf: into.as_mut_ptr().cast::<c_void>(),
        buf_size: STATISTICS_BYTES as u32,
        frame_number: 0,
        config_counter: 0,
    }
}

/// Finished statistics, as [`request_statistics`] copied them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Received {
    /// The frame they are of, as the kernel counts frames.
    pub frame_number: u32,
    /// Bytes filled from the start of the buffer: the saved buffer, then the save-load
    /// statistics after it.
    pub bytes: usize,
}

/// What the kernel's answer says was filled. `InvalidData` when it claims more than the buffer
/// holds, which a kernel keeping its own check never does.
pub(crate) fn received(answer: &StatData) -> io::Result<Received> {
    let bytes = (answer.buf_size as usize)
        .checked_add(SAVE_LOAD_STATISTICS_BYTES)
        .filter(|&n| n <= STATISTICS_BYTES)
        .ok_or(io::ErrorKind::InvalidData)?;
    Ok(Received {
        frame_number: answer.frame_number,
        bytes,
    })
}

/// Hands `image` to the ISP subdevice on `fd` (`VIDIOC_VIN_ISP_LOAD_REG`). The kernel copies all
/// of it during the call and gives it to the block at the next parameter load.
pub fn load(fd: BorrowedFd<'_>, image: &[u8; LOAD_BYTES]) -> io::Result<()> {
    let mut arg = load_arg(image);
    retry(|| {
        // SAFETY: `VIDIOC_VIN_ISP_LOAD_REG` takes a `struct isp_table_reg_map`, which `arg` is
        // (its size is in the request number, and `layout` pins it). The kernel reads `size`
        // bytes at `addr` during this call and never writes there: `addr` is `image`, borrowed
        // for the call, and `size` is its length. `fd` is open for the call.
        unsafe { libc::ioctl(fd.as_raw_fd(), VIDIOC_VIN_ISP_LOAD_REG as _, &raw mut arg) }
    })
}

/// Sets up the statistics subdevice on `fd` (`VIDIOC_VIN_ISP_H3A_CFG`): the kernel makes its
/// statistics buffers. Returns the size it answers with, one saved buffer's bytes.
pub fn configure_statistics(fd: BorrowedFd<'_>) -> io::Result<u32> {
    let mut arg = H3aConfig::default();
    retry(|| {
        // SAFETY: `VIDIOC_VIN_ISP_H3A_CFG` takes a `struct vin_isp_h3a_config`, which `arg` is;
        // it holds no pointer, and the kernel's copy of it is what it writes. `fd` is open.
        unsafe { libc::ioctl(fd.as_raw_fd(), VIDIOC_VIN_ISP_H3A_CFG as _, &raw mut arg) }
    })?;
    Ok(arg.buf_size)
}

/// Turns the statistics on or off on the statistics subdevice on `fd`
/// (`VIDIOC_VIN_ISP_STAT_EN`). On only after [`configure_statistics`].
pub fn enable_statistics(fd: BorrowedFd<'_>, on: bool) -> io::Result<()> {
    let mut arg = u32::from(on);
    retry(|| {
        // SAFETY: `VIDIOC_VIN_ISP_STAT_EN` takes an `unsigned int`, which `arg` is; it holds no
        // pointer. `fd` is open.
        unsafe { libc::ioctl(fd.as_raw_fd(), VIDIOC_VIN_ISP_STAT_EN as _, &raw mut arg) }
    })
}

/// Copies the oldest finished statistics from the statistics subdevice on `fd` into `into`
/// (`VIDIOC_VIN_ISP_STAT_REQ`). `None` when none is finished. `InvalidInput` when the statistics
/// are off.
pub fn request_statistics(
    fd: BorrowedFd<'_>,
    into: &mut [u8; STATISTICS_BYTES],
) -> io::Result<Option<Received>> {
    let mut arg = stat_arg(into);
    let done = retry(|| {
        // SAFETY: `VIDIOC_VIN_ISP_STAT_REQ` takes a `struct vin_isp_stat_data`, which `arg` is.
        // The kernel writes at `buf` only during this call, and only after checking that what it
        // writes fits `buf_size`: `buf` is `into`, exclusively borrowed for the call, and
        // `buf_size` is its length. It reads nothing there. `fd` is open.
        unsafe { libc::ioctl(fd.as_raw_fd(), VIDIOC_VIN_ISP_STAT_REQ as _, &raw mut arg) }
    });
    match done {
        Ok(()) => received(&arg).map(Some),
        Err(err) if err.raw_os_error() == Some(libc::EBUSY) => Ok(None),
        Err(err) => Err(err),
    }
}
