//! The rp1-csi2 receiver's kernel node, `/dev/lemnos-rp1-csi2-<device>` (`kernel/lemnos_rp1_csi2.h`,
//! `kernel/lemnos.h`): its uAPI and safe wrappers over its ioctls, nothing else.
//!
//! The node never takes a memory address. The only addresses the receiver writes are computed in
//! the kernel from the pool of the stream [`Node::start`] names, so nothing here can name memory.
//! Everything here is one ioctl per function, and every one is safe to call.
//!
//! Provenance: ported from `crates/lemnos-linux-sys/src/lemnos_rp1_csi2.rs` on the dev branch, for
//! the frame stream.

#![cfg(target_os = "linux")]

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use lemnos_linux_sys::{ior, iow, iowr};

/// `LEMNOS_RP1_CSI2_ABI`: the node's interface revision this crate speaks.
pub const ABI: u32 = 1;
/// `LEMNOS_RP1_CSI2_INFO_WEDGED`: a stop couldn't be confirmed, so nothing more runs until the
/// block is bound again.
pub const INFO_WEDGED: u32 = 1;
/// `LEMNOS_RP1_CSI2_UNPACK16`: each pixel unpacked to 16 bits, most significant bit aligned.
pub const UNPACK16: u32 = 1;
/// `LEMNOS_RING_STATE_OPEN`: the frame stream is running.
pub const STATE_OPEN: u32 = 0;

/// `struct lemnos_rp1_csi2_info`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Info {
    /// `LEMNOS_RP1_CSI2_ABI` of the loaded module.
    pub abi: u32,
    /// The CSI-2 host's version register.
    pub host_version: u32,
    /// Data lanes wired.
    pub max_lanes: u32,
    /// `LEMNOS_RP1_CSI2_INFO_*`.
    pub flags: u32,
    /// Nonzero between START and STOP.
    pub streaming: u32,
    /// Zero.
    pub reserved: u32,
    /// Frame starts since START.
    pub frames: u64,
    /// Frames completed.
    pub written: u64,
    /// Frame starts with no buffer armed.
    pub dropped: u64,
    /// Frames the block flagged as bad.
    pub errors: u64,
}

/// `struct lemnos_rp1_csi2_start`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Start {
    /// The stream's descriptor, made on this node.
    pub stream_fd: i32,
    /// Data lanes, 1 to [`Info::max_lanes`].
    pub lanes: u32,
    /// Per-lane rate, 80 to 1500 Mbit/s.
    pub mbps: u32,
    /// Virtual channel, 0 to 3.
    pub vc: u32,
    /// CSI-2 data type, 1 to 63.
    pub dt: u32,
    /// Pixels per line, 1 to 65535.
    pub width: u32,
    /// Lines, 1 to 65535.
    pub height: u32,
    /// Bytes per line, a multiple of 16.
    pub stride: u32,
    /// Bytes per frame, a multiple of 16, within a buffer of the stream.
    pub size: u32,
    /// [`UNPACK16`].
    pub flags: u32,
    /// Zero.
    pub reserved: [u32; 2],
}

/// `struct lemnos_stream_create`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StreamCreate {
    block: u32,
    flags: u32,
    buffers: u32,
    buffer_bytes: u32,
    frame_slots: u32,
    job_slots: u32,
    job_bytes: u32,
    pool_fd: i32,
    doorbell_fd: i32,
    fd: i32,
    ring_bytes: u64,
    pool_addr: u64,
}

/// `struct lemnos_stream_status`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamStatus {
    /// `LEMNOS_RING_STATE_*`.
    pub state: u32,
    /// The errno that ended the stream, or 0.
    pub fault: i32,
    /// Frames published.
    pub frames: u64,
    /// No room on the ring.
    pub dropped: u64,
    /// Buffers given back.
    pub returned: u64,
    /// Jobs accepted.
    pub jobs: u64,
}

const IOC_INFO: u32 = ior::<Info>(b'L', 0xc0);
const IOC_STREAM_CREATE: u32 = iowr::<StreamCreate>(b'L', 0xc1);
const IOC_START: u32 = iow::<Start>(b'L', 0xc2);
const IOC_STOP: u32 = lemnos_linux_sys::ioc(0, b'L', 0xc3, 0);
const IOC_STREAM_STATUS: u32 = ior::<StreamStatus>(b'L', 0x84);
/// `sizeof(struct lemnos_stream_job)`: a job's fixed size.
const JOB_BYTES: u32 = 8 + 8 + 16 * 4;

/// What a stream asks of the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamParams {
    /// Buffers in the pool.
    pub buffers: u32,
    /// Bytes per buffer.
    pub buffer_bytes: u32,
    /// Slots on the frame ring, a power of two.
    pub frame_slots: u32,
    /// Slots on the job ring, a power of two.
    pub job_slots: u32,
}

/// An open receiver node. One opener at a time; closing it stops the receiver.
#[derive(Debug)]
pub struct Node {
    file: File,
}

impl Node {
    /// Opens the node at `path`, refusing another interface revision.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(path)?;
        let node = Self { file };
        if node.info()?.abi != ABI {
            return Err(io::ErrorKind::Unsupported.into());
        }
        Ok(node)
    }

    fn ioctl<T>(&self, request: u32, arg: *mut T) -> io::Result<libc::c_int> {
        ioctl(self.file.as_fd(), request, arg.cast())
    }

    /// The receiver's state, from the kernel's own counters.
    pub fn info(&self) -> io::Result<Info> {
        let mut info = Info::default();
        self.ioctl(IOC_INFO, &raw mut info)?;
        Ok(info)
    }

    /// Makes a frame stream on this block, its buffers in `pool`, and returns its descriptor.
    pub fn stream_create(
        &self,
        params: &StreamParams,
        pool: BorrowedFd<'_>,
    ) -> io::Result<OwnedFd> {
        let mut create = StreamCreate {
            block: 0,
            flags: 0,
            buffers: params.buffers,
            buffer_bytes: params.buffer_bytes,
            frame_slots: params.frame_slots,
            job_slots: params.job_slots,
            job_bytes: JOB_BYTES,
            pool_fd: pool.as_raw_fd(),
            doorbell_fd: -1,
            fd: -1,
            ring_bytes: 0,
            pool_addr: 0,
        };
        self.ioctl(IOC_STREAM_CREATE, &raw mut create)?;
        // SAFETY: a successful create returns a new close-on-exec descriptor that nothing else
        // owns.
        Ok(unsafe { OwnedFd::from_raw_fd(create.fd) })
    }

    /// Starts receiving into the stream `start.stream_fd` names.
    pub fn start(&self, start: &Start) -> io::Result<()> {
        let mut args = *start;
        self.ioctl(IOC_START, &raw mut args).map(|_| ())
    }

    /// Stops. Returns only when the block's DMA has stopped, or it could not be confirmed.
    pub fn stop(&self) -> io::Result<()> {
        ioctl(self.file.as_fd(), IOC_STOP, std::ptr::null_mut()).map(|_| ())
    }
}

/// How a stream the node made is doing.
pub fn stream_status(stream: BorrowedFd<'_>) -> io::Result<StreamStatus> {
    let mut status = StreamStatus::default();
    ioctl(stream, IOC_STREAM_STATUS, (&raw mut status).cast())?;
    Ok(status)
}

fn ioctl(fd: BorrowedFd<'_>, request: u32, arg: *mut libc::c_void) -> io::Result<libc::c_int> {
    loop {
        // SAFETY: every `request` in this crate is paired, by the callers above, with an `arg`
        // that points at the structure the kernel expects (or is null for one taking none), live
        // for the call. `fd` is live.
        let ret = unsafe { libc::ioctl(fd.as_raw_fd(), request as _, arg) };
        if ret >= 0 {
            return Ok(ret);
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
    use std::mem::{offset_of, size_of};

    #[test]
    fn layouts_and_requests_match_the_kernel_headers() {
        assert_eq!(size_of::<Info>(), 56);
        assert_eq!(offset_of!(Info, frames), 24);
        assert_eq!(offset_of!(Info, errors), 48);
        assert_eq!(size_of::<Start>(), 48);
        assert_eq!(offset_of!(Start, flags), 36);
        assert_eq!(size_of::<StreamCreate>(), 56);
        assert_eq!(offset_of!(StreamCreate, fd), 36);
        assert_eq!(offset_of!(StreamCreate, ring_bytes), 40);
        assert_eq!(size_of::<StreamStatus>(), 40);
        assert_eq!(IOC_INFO, 0x8038_4cc0);
        assert_eq!(IOC_STREAM_CREATE, 0xc038_4cc1);
        assert_eq!(IOC_START, 0x4030_4cc2);
        assert_eq!(IOC_STOP, 0x0000_4cc3);
        assert_eq!(IOC_STREAM_STATUS, 0x8028_4c84);
        assert_eq!(JOB_BYTES, 80);
    }
}
