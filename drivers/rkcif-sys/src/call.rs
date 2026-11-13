//! The two requests, on a capture node's descriptor. Each passes the kernel one `int` this
//! call owns, which is what the request number says it copies.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};

use crate::{MemoryMode, RKCIF_CMD_GET_CSI_MEMORY_MODE, RKCIF_CMD_SET_CSI_MEMORY_MODE};

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

/// Sets how the capture node on `fd` lays raw samples out (`RKCIF_CMD_SET_CSI_MEMORY_MODE`).
/// The vendor driver does not work its format out again here, so set the format after.
pub fn set_memory_mode(fd: BorrowedFd<'_>, mode: MemoryMode) -> io::Result<()> {
    let mut arg: libc::c_int = mode as libc::c_int;
    retry(|| {
        // SAFETY: `RKCIF_CMD_SET_CSI_MEMORY_MODE` is `_IOW` of one `int`, so the kernel copies
        // `size_of::<c_int>()` bytes from the pointer during the call: `arg` is that `int`,
        // owned here and alive for the call. `fd` is open for the call.
        unsafe {
            libc::ioctl(
                fd.as_raw_fd(),
                RKCIF_CMD_SET_CSI_MEMORY_MODE as _,
                &raw mut arg,
            )
        }
    })
}

/// How the capture node on `fd` lays raw samples out now (`RKCIF_CMD_GET_CSI_MEMORY_MODE`).
/// `InvalidData` for an answer that is no mode.
pub fn memory_mode(fd: BorrowedFd<'_>) -> io::Result<MemoryMode> {
    let mut arg: libc::c_int = -1;
    retry(|| {
        // SAFETY: `RKCIF_CMD_GET_CSI_MEMORY_MODE` is `_IOR` of one `int`, so the kernel copies
        // `size_of::<c_int>()` bytes to the pointer during the call: `arg` is that `int`, owned
        // here and alive for the call. `fd` is open for the call.
        unsafe {
            libc::ioctl(
                fd.as_raw_fd(),
                RKCIF_CMD_GET_CSI_MEMORY_MODE as _,
                &raw mut arg,
            )
        }
    })?;
    MemoryMode::from_raw(arg).ok_or_else(|| io::ErrorKind::InvalidData.into())
}
