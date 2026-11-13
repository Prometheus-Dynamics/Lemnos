//! What a frame stream needs from Linux: receiving its descriptors over a socket, and telling the
//! kernel around each CPU access to a dma-buf.
//!
//! The rings, the pool, the holds and the doorbell are the shm family's (`docs/ring.md`), which map
//! their own memory safely, so this crate has no mapping of its own. With the `client` feature (on
//! by default) [`LinuxStreams`] implements `StreamPlatform`, which is what a program passes to
//! `lemnos-client` for its streams. All the `unsafe` is in `lemnos-linux-sys`.

#![cfg(target_os = "linux")]

use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::time::Duration;

use lemnos_linux_sys::dma_buf::{SYNC_END, SYNC_READ, SYNC_START, SYNC_WRITE, sync};
use lemnos_linux_sys::poll::{POLLERR, POLLHUP, POLLIN, POLLNVAL, PollFd, poll_many};
use lemnos_linux_sys::scm;

/// Which way a CPU access to a dma-buf goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// Plain Linux: `SCM_RIGHTS` and `DMA_BUF_IOCTL_SYNC`.
#[derive(Debug, Clone, Copy, Default)]
pub struct LinuxStreams;

impl LinuxStreams {
    /// Reads from `socket` into `buf`, appending any descriptors that arrived to `fds`. Returns
    /// the bytes read, 0 at end of file.
    pub fn recv_with_descriptors(
        &self,
        socket: BorrowedFd<'_>,
        buf: &mut [u8],
        fds: &mut Vec<OwnedFd>,
    ) -> io::Result<usize> {
        scm::recv_with_fds(socket, buf, fds)
    }

    /// Waits until `fd` is readable or `timeout` passes. `false` on the timeout and on `EINTR`:
    /// the caller looks at its ring and waits again with what is left of its deadline. The wait is
    /// one `ppoll`, which a signal never restarts, so no time is lost to a retry here.
    pub fn wait_readable(&self, fd: BorrowedFd<'_>, timeout: Duration) -> io::Result<bool> {
        let mut fds = [PollFd::new(fd, POLLIN)];
        poll_many(&mut fds, Some(timeout))?;
        let events = fds[0].revents();
        if events & (POLLERR | POLLHUP | POLLNVAL) != 0 {
            return Err(io::Error::other("the descriptor failed or hung up"));
        }
        Ok(events & POLLIN != 0)
    }

    /// Starts (`begin`) or ends an access of `access` to the dma-buf `fd`.
    pub fn dma_sync(&self, fd: BorrowedFd<'_>, access: Access, begin: bool) -> io::Result<()> {
        let direction = match access {
            Access::Read => SYNC_READ,
            Access::Write => SYNC_WRITE,
        };
        let edge = if begin { SYNC_START } else { SYNC_END };
        sync(fd, edge | direction)
    }
}

#[cfg(feature = "client")]
impl lemnos_client::StreamPlatform for LinuxStreams {
    fn recv_with_descriptors(
        &self,
        socket: BorrowedFd<'_>,
        buf: &mut [u8],
        fds: &mut Vec<OwnedFd>,
    ) -> io::Result<usize> {
        LinuxStreams::recv_with_descriptors(self, socket, buf, fds)
    }

    fn wait_readable(&self, fd: BorrowedFd<'_>, timeout: Duration) -> io::Result<bool> {
        LinuxStreams::wait_readable(self, fd, timeout)
    }

    fn dma_sync(
        &self,
        fd: BorrowedFd<'_>,
        access: lemnos_client::Access,
        begin: bool,
    ) -> io::Result<()> {
        let access = match access {
            lemnos_client::Access::Read => Access::Read,
            lemnos_client::Access::Write => Access::Write,
        };
        LinuxStreams::dma_sync(self, fd, access, begin)
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd;

    use super::*;

    #[test]
    fn a_descriptor_that_is_not_a_dma_buf_is_refused_by_the_kernel_and_nothing_panics() {
        let file = std::fs::File::open("/dev/null").expect("dev null");
        let _ = LinuxStreams.dma_sync(file.as_fd(), Access::Read, true);
    }
}
