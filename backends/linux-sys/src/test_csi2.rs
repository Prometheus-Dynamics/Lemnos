//! The fake camera receiver of the QEMU harness's test module (`testing/qemu/test_block`, the
//! `/dev/lemnos-test-csi2` node): one request, which fires a status word the fake block acts on.
//! Test support only: a frame is a start (`FS`, which latches the buffer's address) and an end
//! (`FE_ACK`, which fills the latched buffer with the next pattern).

use std::io;
use std::os::fd::BorrowedFd;

use crate::ioctl::Ioctl;

/// `TCSI_FS`: a frame starts, latching the buffer address the block was given.
pub const FS: u32 = 0x1;
/// `TCSI_FE_ACK`: the frame ends; the latched buffer is filled.
pub const FE_ACK: u32 = 0x100;

/// `TCSI_IOC_FIRE`, `_IOW('T', 1, __u32)`.
const FIRE: Ioctl<u32> = Ioctl::new(crate::ioctl::iow::<u32>(b'T', 1), "TCSI_IOC_FIRE");

/// Fires `status` on the fake receiver's node, and returns once the request was taken.
pub fn fire(fd: BorrowedFd<'_>, mut status: u32) -> io::Result<()> {
    FIRE.call(fd, &mut status).map(|_| ())
}
