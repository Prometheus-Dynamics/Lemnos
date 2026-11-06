//! Helpers for capture backends built on [`VideoDevice`]: waiting for a filled capture buffer
//! only, and exporting buffers as dma-bufs with chosen access.

use std::os::fd::{AsFd, OwnedFd};
use std::time::Duration;

use lemnos_linux_sys::poll::{POLLIN, Ready};
use lemnos_linux_sys::v4l2::device::{export_buffer, v4l2_exportbuffer, zeroed};

use super::{BufType, VideoDevice};
use crate::node;
use crate::{Error, Result};

/// Access mode of an exported dma-buf descriptor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DmaBufAccess {
    /// `O_RDONLY`: importers can read (and map read-only).
    #[default]
    ReadOnly,
    /// `O_RDWR`: importers can also map it writable.
    ReadWrite,
}

impl DmaBufAccess {
    /// The open flags of the descriptor (close-on-exec, and the access mode).
    pub(crate) fn open_flags(self) -> u32 {
        let access = match self {
            DmaBufAccess::ReadOnly => libc::O_RDONLY,
            DmaBufAccess::ReadWrite => libc::O_RDWR,
        };
        (libc::O_CLOEXEC | access) as u32
    }
}

impl VideoDevice {
    /// Waits until a buffer can be dequeued from a capture queue (`POLLIN`), or the timeout
    /// passes (an empty [`Ready`]). `None` waits forever. `error`/`hangup` report a stopped
    /// queue or a device that went away.
    pub fn wait_readable(&self, timeout: Option<Duration>) -> Result<Ready> {
        node::wait_events(self.as_fd(), POLLIN, timeout)
    }

    /// Exports one plane of an MMAP buffer as a close-on-exec dma-buf with the given access
    /// (`VIDIOC_EXPBUF`). [`VideoDevice::export_buffer`] is the read-write form.
    pub fn export_buffer_with(
        &self,
        buf_type: BufType,
        index: u32,
        plane: u32,
        access: DmaBufAccess,
    ) -> Result<OwnedFd> {
        let mut raw: v4l2_exportbuffer = zeroed();
        raw.type_ = buf_type.to_raw();
        raw.index = index;
        raw.plane = plane;
        raw.flags = access.open_flags();
        export_buffer(self.as_fd(), &mut raw)
            .map_err(|source| Error::ioctl_io("VIDIOC_EXPBUF", source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_flags_match_open_flags() {
        let ro = DmaBufAccess::ReadOnly.open_flags();
        assert_eq!(ro & libc::O_ACCMODE as u32, libc::O_RDONLY as u32);
        assert_ne!(ro & libc::O_CLOEXEC as u32, 0);
        let rw = DmaBufAccess::ReadWrite.open_flags();
        assert_eq!(rw & libc::O_ACCMODE as u32, libc::O_RDWR as u32);
        assert_eq!(DmaBufAccess::default(), DmaBufAccess::ReadOnly);
    }
}
